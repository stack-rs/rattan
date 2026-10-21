// SPDX-License-Identifier: GPL-2.0
/*
 * Rattan Virtual NIC
 *
 * Copyright (C) 2025 Rattan Project
 *
 * This module implements a virtual network device with:
 * - Miscdevice interface for userspace control
 * - Dynamic netdev creation on open
 * - AF_XDP-style memory management
 */

#include "rattan_vnic_uapi.h"
#define pr_fmt(fmt) "rvnic: " fmt

#include <linux/atomic.h>
#include <linux/etherdevice.h>
#include <linux/fs.h>
#include <linux/hrtimer.h>
#include <linux/idr.h>
#include <linux/init.h>
#include <linux/jiffies.h>
#include <linux/kernel.h>
#include <linux/kernel_stat.h>
#include <linux/ktime.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/netdevice.h>
#include <linux/percpu.h>
#include <linux/poll.h>
#include <linux/ratelimit.h>
#include <linux/sched/clock.h>
#include <linux/slab.h>
#include <linux/smp.h>
#include <linux/uaccess.h>
#include <linux/version.h>
#include <linux/vmalloc.h>
#include <linux/workqueue.h>

#include "rattan_vnic.h"

/*
 * Kernel version compatibility for page pinning APIs
 * pin_user_pages_fast was introduced in 5.6
 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(5, 6, 0)
#define rattan_pin_pages(addr, nr, flags, pages) pin_user_pages_fast(addr, nr, flags, pages)
#define rattan_unpin_pages(pages, nr) unpin_user_pages(pages, nr)
#else
#define rattan_pin_pages(addr, nr, flags, pages) get_user_pages_fast(addr, nr, flags, pages)
static void rattan_unpin_pages(struct page **pages, unsigned long nr) {
    unsigned long i;

    for (i = 0; i < nr; i++)
        put_page(pages[i]);
}
#endif

/*
 * Kernel version compatibility for netif_napi_add
 * The weight parameter was removed in kernel 6.1
 */
#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 1, 0)
#define rattan_netif_napi_add(dev, napi, poll) netif_napi_add(dev, napi, poll, NAPI_POLL_WEIGHT)
#else
#define rattan_netif_napi_add(dev, napi, poll) netif_napi_add(dev, napi, poll)
#endif

/*
 * Kernel version compatibility for skb_gso_segment
 * Moved from netdevice.h to net/gso.h in kernel 6.4.10
 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 4, 10)
#include <net/gso.h>
#endif

/*
 * Kernel version compatibility for per-cpu device stats
 * - The core allocates dev->tstats for NETDEV_PCPU_STAT_TSTATS since 6.7;
 *   before that the driver does it in ndo_init/ndo_uninit.
 * - Per-cpu drop counters (dev->core_stats) since 5.18; before that the
 *   atomic dev->{rx,tx}_dropped.
 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 7, 0)
#define RATTAN_CORE_TSTATS
#endif

#if LINUX_VERSION_CODE >= KERNEL_VERSION(5, 18, 0)
#define rattan_tx_dropped_inc(dev) dev_core_stats_tx_dropped_inc(dev)
#define rattan_rx_dropped_inc(dev) dev_core_stats_rx_dropped_inc(dev)
#else
#define rattan_tx_dropped_inc(dev) atomic_long_inc(&(dev)->tx_dropped)
#define rattan_rx_dropped_inc(dev) atomic_long_inc(&(dev)->rx_dropped)
#endif

/*
 * Kernel version compatibility for hrtimer_init / hrtimer_setup
 * hrtimer_setup was introduced since 6.13 and hrtimer_init was removed from 6.15
 * 6.14 is a version that these two functions coexist
 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 14, 0)
#define rattan_hrtimer_setup(timer, restart, clock, mode) hrtimer_setup(timer, restart, clock, mode) 
#else
static inline void rattan_hrtimer_setup(struct hrtimer *timer,
                                       enum hrtimer_restart (*function)(struct hrtimer *),
                                       clockid_t which_clock,
                                       enum hrtimer_mode mode)
{
    hrtimer_init(timer, which_clock, mode);
    timer->function = function;
}
#endif

static int max_devices = 256;
module_param(max_devices, int, 0644);
MODULE_PARM_DESC(max_devices, "Maximum number of rattan devices (default: 256)");

static int num_queues = 1;
module_param(num_queues, int, 0644);
MODULE_PARM_DESC(num_queues, "Number of TX/RX queues per rattan device (default: 1)");

static unsigned int debug_stats_interval_ms = 0;
module_param(debug_stats_interval_ms, uint, 0644);
MODULE_PARM_DESC(debug_stats_interval_ms,
                 "Periodic per-queue debug stats dump interval in ms (0 = disabled). "
                 "The counters only count while this is nonzero.");

/*
 * Per-cpu debug counters (struct rattan_queue_dbg). A plain branch on a
 * read-mostly global when the dump is off; no locked op or shared line when on.
 */
#define RATTAN_DBG_ADD(queue, field, n)                                                            \
    do {                                                                                           \
        if (unlikely(READ_ONCE(debug_stats_interval_ms)))                                          \
            this_cpu_add((queue)->dbg->field, (n));                                                \
    } while (0)
#define RATTAN_DBG_INC(queue, field) RATTAN_DBG_ADD(queue, field, 1)

/* ================== Debug: ndo_start_xmit latency bisection ==================
 *
 * Splits the xmit path into stages and warns, with a per-stage breakdown, when
 * the total exceeds slow_tx_ns. Set slow_tx_ns=0 to disable: each checkpoint
 * then costs one branch and nothing else.
 *
 * Remove this block (and its call sites) once the slow-TX question is settled.
 */
static unsigned int slow_tx_ns = 0;
module_param(slow_tx_ns, uint, 0644);
MODULE_PARM_DESC(slow_tx_ns, "Warn when ndo_start_xmit exceeds this many ns (0 = disabled)");

enum {
    XT_START,   /* entry to ndo_start_xmit */
    XT_RESOLVE, /* queue + rings + umem resolved */
    XT_ALLOC,   /* unused since the slab entry went away; kept for log compatibility */
    XT_CHUNK,   /* chunk obtained: COMP/FILL pop + DROP-ring token release */
    XT_ENTRY,   /* header copied to UMEM */
    XT_ORPHAN,  /* skb_orphan + nf_reset_ct, i.e. the socket destructor */
    XT_HASH,    /* bound to the chunk's pending slot, i.e. the token insert */
    XT_PUSH,    /* descriptor published to the RX ring */
    XT_NOTIFY,  /* rattan_rx_notify returned, i.e. the userspace wake-up */
    XT_NR
};

static const char *const xt_names[XT_NR] = {
    "start", "resolve", "alloc", "chunk", "entry", "orphan", "hash", "push", "notify",
};

/* Which branch rattan_rx_notify() took */
#define RATTAN_NOTIFY_NONE 0 /* not reached (packet dropped) */
#define RATTAN_NOTIFY_ARM 1  /* first of a batch: hrtimer armed */
#define RATTAN_NOTIFY_MID 2  /* mid-batch: nothing done */
#define RATTAN_NOTIFY_WAKE 3 /* batch full: wake_up performed */

/*
 * local_clock() is only comparable within one CPU, which is fine: ndo_start_xmit
 * runs with BH (hence preemption) disabled, so every sample below is taken on
 * the same CPU. For a GSO skb the per-stage fields describe the LAST segment,
 * while nsegs counts all of them.
 */
struct rattan_xmit_trace {
    u64 t[XT_NR];
    unsigned long irqs_start;
    u32 nsegs;
    u8 notify_path;
    bool recycled; /* a DROP-ring token was released during this call */
};

#define XT(tr, idx)                                                                                \
    do {                                                                                           \
        if (tr)                                                                                    \
            (tr)->t[idx] = local_clock();                                                          \
    } while (0)

/* 200 lines/s is enough to see a distribution without flooding the log */
static DEFINE_RATELIMIT_STATE(slow_tx_rs, HZ, 200);

/*
 * Format " name=delta" for every recorded stage in t[1..last], each delta taken
 * against the nearest earlier recorded stage. t[0] is the start timestamp.
 */
static void rattan_format_stages(const u64 *t, int last, const char *const *names, char *buf,
                                 size_t size) {
    size_t n = 0;
    int i, j;

    buf[0] = '\0';
    for (i = 1; i <= last; i++) {
        if (!t[i])
            continue;
        for (j = i - 1; j >= 0; j--) {
            if (t[j]) {
                n += scnprintf(buf + n, size - n, " %s=%llu", names[i], t[i] - t[j]);
                break;
            }
        }
    }
}

static void rattan_report_slow_tx(struct rattan_xmit_trace *tr, struct rattan_queue *queue,
                                  const char *path) {
    unsigned long irqs;
    char buf[192];
    u64 total;
    int i, last = XT_START;

    if (!tr || !tr->t[XT_START])
        return;

    irqs = kstat_this_cpu->irqs_sum;

    for (i = XT_START + 1; i < XT_NR; i++)
        if (tr->t[i])
            last = i;

    /* An early drop records no stages; still report the total. */
    total = (last == XT_START ? local_clock() : tr->t[last]) - tr->t[XT_START];
    if (total < slow_tx_ns)
        return;

    rattan_format_stages(tr->t, last, xt_names, buf, sizeof(buf));

    if (!__ratelimit(&slow_tx_rs))
        return;

    pr_warn("slow tx[%s] rattan%d:q%u %llu ns cpu=%d segs=%u notify=%u "
            "recycled=%d hardirqs=%lu |%s\n",
            path, queue ? queue->ctx->dev_id : -1, queue ? queue->qid : 0, total,
            smp_processor_id(), tr->nsegs, tr->notify_path, tr->recycled,
            irqs - tr->irqs_start, buf);
}
/* ============== End debug: ndo_start_xmit latency bisection ============== */

/* ================== Debug: NAPI per-packet latency bisection ==================
 *
 * Same idea as the xmit trace, for one iteration of the rattan_napi_poll loop.
 * Stage names are distinct from the xmit ones so the two can be aggregated from
 * one log. Set slow_rx_ns=1 to sample every packet (subject to the ratelimit).
 */
static unsigned int slow_rx_ns = 0;
module_param(slow_rx_ns, uint, 0644);
MODULE_PARM_DESC(slow_rx_ns, "Warn when one NAPI packet exceeds this many ns (0 = disabled)");

enum {
    NT_START,   /* top of the loop iteration */
    NT_PEEK,    /* COMP space check + TX descriptor peeked */
    NT_LOOKUP,  /* token's pending slot claimed */
    NT_UNLINK,  /* descriptor consumed, slot emptied */
    NT_PREP,    /* header copy-back + eth_type_trans */
    NT_STACK,   /* netif_receive_skb returned */
    NT_RELEASE, /* chunk(s) completed, rings reference dropped */
    NT_NR
};

static const char *const nt_names[NT_NR] = {
    "nstart", "peek", "lookup", "unlink", "prep", "stack", "release",
};

struct rattan_napi_trace {
    u64 t[NT_NR];
    unsigned long irqs_start;
};

#define NT(tr, idx) XT(tr, idx)

static DEFINE_RATELIMIT_STATE(slow_rx_rs, HZ, 200);

static void rattan_napi_trace_begin(struct rattan_napi_trace *tr) {
    memset(tr, 0, sizeof(*tr));
    tr->irqs_start = kstat_this_cpu->irqs_sum;
    tr->t[NT_START] = local_clock();
}

static void rattan_report_slow_rx(struct rattan_napi_trace *tr, struct rattan_queue *queue,
                                  const char *path) {
    char buf[192];
    u64 total;
    int i, last = NT_START;

    if (!tr || !tr->t[NT_START])
        return;

    for (i = NT_START + 1; i < NT_NR; i++)
        if (tr->t[i])
            last = i;
    if (last == NT_START)
        return;

    total = tr->t[last] - tr->t[NT_START];
    if (total < slow_rx_ns)
        return;

    if (!__ratelimit(&slow_rx_rs))
        return;

    rattan_format_stages(tr->t, last, nt_names, buf, sizeof(buf));
    pr_warn("slow rx[%s] rattan%d:q%u %llu ns cpu=%d hardirqs=%lu |%s\n", path,
            queue->ctx->dev_id, queue->qid, total, smp_processor_id(),
            kstat_this_cpu->irqs_sum - tr->irqs_start, buf);
}
/* ============== End debug: NAPI per-packet latency bisection ============== */

/* Device management - use IDA for unique device IDs */
static DEFINE_IDA(rattan_ida);

/* Every live UMEM, so GC can find the pending slots. Protected by rattan_umems_lock. */
static LIST_HEAD(rattan_umems);
static DEFINE_MUTEX(rattan_umems_lock);

/*
 * Live token count, for the debug dump only. Per-cpu so that the xmit CPU
 * (insert) and the NAPI CPU (release) do not bounce a shared counter.
 */
static DEFINE_PER_CPU(long, rattan_pending_tokens);

/* Periodic GC workqueue */
static struct delayed_work gc_work;

/* Deferred rattan_rings cleanup; drained on module exit */
static struct workqueue_struct *rattan_cleanup_wq;
static bool gc_work_initialized;

static const struct file_operations rattan_vnic_fops;

static int rattan_vnic_open(struct net_device *dev);
static int rattan_vnic_stop(struct net_device *dev);
static netdev_tx_t rattan_vnic_start_xmit(struct sk_buff *skb, struct net_device *dev);

static bool fill_ring_pop(struct rattan_fill_ring *fill, u64 *addr);
static bool rx_ring_has_space(struct rattan_rx_ring *rx);
static int rx_ring_push(struct rattan_rx_ring *rx, struct rattan_desc *desc);
static u32 comp_ring_push_many(struct rattan_comp_ring *comp, const u64 *addrs, u32 n);
static u32 comp_ring_free(struct rattan_comp_ring *comp);
static bool comp_ring_pop(struct rattan_comp_ring *comp, u64 *addr);
static bool release_token(struct rattan_umem *umem, u64 token);
static bool rattan_recycle_drop_token(struct rattan_umem *umem, struct rattan_drop_ring *drop);
static bool try_get_free_addr(struct rattan_queue *queue, struct rattan_rings *rings,
                              struct rattan_umem *umem, u64 *addr,
                              struct rattan_xmit_trace *tr);
static bool tx_ring_peek(struct rattan_tx_ring *tx, struct rattan_desc *desc);
static void tx_ring_advance(struct rattan_tx_ring *tx);
static int rattan_napi_poll(struct napi_struct *napi, int budget);
static enum hrtimer_restart rattan_rx_timer_cb(struct hrtimer *timer);
static struct rattan_queue *rattan_get_queue(struct rattan_vnic_ctx *ctx, u32 qid);
static struct rattan_queue *rattan_get_xmit_queue(struct rattan_vnic_ctx *ctx, struct sk_buff *skb);
static struct rattan_rings *rattan_get_rings(struct rattan_queue *queue);
static void rattan_put_rings(struct rattan_rings *rings);
static bool rattan_complete_chunk(struct rattan_queue *queue, struct rattan_rings *rings, u64 addr);
static void rattan_debug_dump_queue_stats(struct rattan_queue *queue);
static void rattan_umem_drain(struct rattan_umem *umem);
static u64 rattan_slot_insert(struct rattan_umem *umem, u32 idx, struct sk_buff *skb,
                              struct rattan_rings *rings, u16 qid);

/*
 * Is addr (a UMEM offset from userspace: FILL, TX descriptors, CLONE) the
 * start of a chunk? umem->len is a multiple of chunk_size, so an aligned
 * addr below len names a whole chunk. Never test "addr + chunk_size <= len":
 * a u64 addr near 2^64 wraps that sum around to pass.
 */
static inline bool rattan_chunk_valid(struct rattan_umem *umem, u64 addr) {
    return (addr & (u64)(umem->chunk_size - 1)) == 0 && addr < umem->len;
}

/* ========== Net Device Operations ========== */

#ifndef RATTAN_CORE_TSTATS
static int rattan_vnic_ndo_init(struct net_device *dev) {
    dev->tstats = netdev_alloc_pcpu_stats(struct pcpu_sw_netstats);
    return dev->tstats ? 0 : -ENOMEM;
}

static void rattan_vnic_ndo_uninit(struct net_device *dev) {
    free_percpu(dev->tstats);
}
#endif

static const struct net_device_ops rattan_netdev_ops = {
#ifndef RATTAN_CORE_TSTATS
    .ndo_init = rattan_vnic_ndo_init,
    .ndo_uninit = rattan_vnic_ndo_uninit,
#endif
    .ndo_open = rattan_vnic_open,
    .ndo_stop = rattan_vnic_stop,
    .ndo_start_xmit = rattan_vnic_start_xmit,
    .ndo_get_stats64 = dev_get_tstats64,
};

static int rattan_vnic_open(struct net_device *dev) {
    struct rattan_vnic_ctx *ctx = netdev_priv(dev);
    u16 qid;

    for (qid = 0; qid < ctx->num_queues; qid++)
        netif_start_subqueue(dev, qid);

    return 0;
}

static int rattan_vnic_stop(struct net_device *dev) {
    struct rattan_vnic_ctx *ctx = netdev_priv(dev);
    u16 qid;

    for (qid = 0; qid < ctx->num_queues; qid++)
        netif_stop_subqueue(dev, qid);

    return 0;
}

/*
 * RX wake-up coalescing timer callback
 *
 * Called when the coalescing timeout expires. Wakes up userspace if there
 * are pending packets that haven't been notified yet.
 */
static enum hrtimer_restart rattan_rx_timer_cb(struct hrtimer *timer) {
    struct rattan_queue *queue = container_of(timer, struct rattan_queue, rx_timer);
    struct rattan_vnic_ctx *ctx = queue->ctx;

    /* Wake up if there are pending packets */
    if (atomic_xchg(&queue->rx_pending, 0) > 0) {
        if (waitqueue_active(&queue->wait))
            wake_up_interruptible_poll(&queue->wait, EPOLLIN | EPOLLRDNORM);
        if (waitqueue_active(&ctx->wait))
            wake_up_interruptible_poll(&ctx->wait, EPOLLIN | EPOLLRDNORM);
    }

    return HRTIMER_NORESTART;
}

/*
 * Try to wake up userspace with coalescing
 *
 * Called after pushing packets to the RX ring. Implements batching:
 * - If batch threshold reached, wake immediately
 * - Otherwise, arm timer to wake after timeout (if not already armed)
 */
static void rattan_rx_notify(struct rattan_queue *queue, struct rattan_xmit_trace *tr) {
    struct rattan_vnic_ctx *ctx = queue->ctx;
    int pending;

    RATTAN_DBG_INC(queue, rx_notify);
    pending = atomic_inc_return(&queue->rx_pending);

    if (tr)
        tr->notify_path = pending >= RATTAN_RX_BATCH_SIZE ? RATTAN_NOTIFY_WAKE
                          : pending == 1                 ? RATTAN_NOTIFY_ARM
                                                         : RATTAN_NOTIFY_MID;

    if (pending >= RATTAN_RX_BATCH_SIZE) {
        /* Batch full - wake immediately */
        atomic_set(&queue->rx_pending, 0);
        hrtimer_try_to_cancel(&queue->rx_timer);
        if (waitqueue_active(&queue->wait))
            wake_up_interruptible_poll(&queue->wait, EPOLLIN | EPOLLRDNORM);
        if (waitqueue_active(&ctx->wait))
            wake_up_interruptible_poll(&ctx->wait, EPOLLIN | EPOLLRDNORM);
    } else if (pending == 1) {
        /* First packet in new batch - arm timer */
        hrtimer_start(&queue->rx_timer, ns_to_ktime(RATTAN_RX_TIMEOUT_NS), HRTIMER_MODE_REL_SOFT);
    }
    /* Otherwise timer is already armed, just wait */
}

/*
 * Transmit a single packet (or GSO segment) to userspace via RX ring
 * Returns:
 *   < 0: error
 *   0: success
 */
static int rattan_xmit_one(struct sk_buff *skb, struct net_device *dev, struct rattan_queue *queue,
                           struct rattan_rings *rings, struct rattan_umem *umem,
                           struct rattan_xmit_trace *tr) {
    struct rattan_desc desc;
    u64 chunk_addr;
    u64 token;
    void *chunk_ptr;
    u32 copy_len;
    u32 pkt_len;

    /* Check RX ring has space */
    if (!rx_ring_has_space(rings->rx)) {
        RATTAN_DBG_INC(queue, xmit_rx_full);
        pr_warn_ratelimited("rattan%d:q%u xmit blocked: RX ring full\n", queue->ctx->dev_id,
                            queue->qid);
        return -ENOSPC;
    }

    /* Get chunk from COMP or FILL ring */
    if (!try_get_free_addr(queue, rings, umem, &chunk_addr, tr)) {
        RATTAN_DBG_INC(queue, xmit_fill_empty);
        pr_warn_ratelimited("rattan%d:q%u xmit blocked: no chunk available\n", queue->ctx->dev_id,
                            queue->qid);
        return -ENOBUFS;
    }
    XT(tr, XT_CHUNK);

    /* Validate chunk address */
    if (!rattan_chunk_valid(umem, chunk_addr)) {
        RATTAN_DBG_INC(queue, xmit_invalid_addr);
        pr_warn_ratelimited("rattan%d:q%u invalid chunk addr 0x%llx\n", queue->ctx->dev_id,
                            queue->qid, chunk_addr);
        return -EINVAL;
    }

    /*
     * Copy header to UMEM chunk. RATTAN_HEADER_SIZE is 0 by design; the
     * guard lets the compiler drop the (out-of-line) copy entirely.
     */
    copy_len = 0;
    if (RATTAN_HEADER_SIZE) {
        copy_len = min_t(u32, skb->len, RATTAN_HEADER_SIZE);
        chunk_ptr = (u8 *)umem->mapped + chunk_addr + umem->headroom;
        skb_copy_bits(skb, 0, chunk_ptr, copy_len);
    }
    XT(tr, XT_ENTRY);

    /*
     * Finish with the SKB *before* it becomes reachable through a token.
     *
     * Once the slot is LIVE, whoever presents the token (normally NAPI on
     * another CPU, after userspace hands it back via the TX ring) may pass
     * the SKB to netif_receive_skb(), after which the stack owns it and may
     * free it. Any access to skb after the insert races with that and can be
     * a use-after-free.
     *
     * Orphan the skb, or it will still occupy the TSQ credit
     * on the sender side.
     *
     * Copied from https://elixir.bootlin.com/linux/v6.12.74/source/drivers/net/tun.c#L1121-L1126
     */
    pkt_len = skb->len;
    skb_orphan(skb);
    nf_reset_ct(skb);
    XT(tr, XT_ORPHAN);

    /* Bind the SKB to the chunk; the slot owns this rings reference */
    refcount_inc(&rings->refcount);
    token = rattan_slot_insert(umem, chunk_addr >> umem->chunk_shift, skb, rings, queue->qid);
    if (!token) {
        rattan_put_rings(rings);
        RATTAN_DBG_INC(queue, xmit_invalid_addr);
        pr_warn_ratelimited("rattan%d:q%u chunk 0x%llx is still bound to a live token\n",
                            queue->ctx->dev_id, queue->qid, chunk_addr);
        return -EBUSY;
    }
    XT(tr, XT_HASH);

    /* Build descriptor for userspace */
    desc.addr = chunk_addr;
    desc.len = copy_len;
    desc.options = pkt_len & 0xffff;
    desc.token = token;
    desc.timestamp = ktime_get_ns();

    dev_sw_netstats_tx_add(dev, 1, pkt_len);

    rx_ring_push(rings->rx, &desc);
    RATTAN_DBG_INC(queue, xmit_ok);
    XT(tr, XT_PUSH);

    return 0;
}

static netdev_tx_t rattan_vnic_start_xmit(struct sk_buff *skb, struct net_device *dev) {
    struct rattan_vnic_ctx *ctx = netdev_priv(dev);
    struct rattan_queue *queue = NULL;
    struct rattan_rings *rings;
    struct rattan_umem *umem;
    struct sk_buff *segs, *next;
    struct rattan_xmit_trace trace;
    struct rattan_xmit_trace *tr = NULL;
    int ret;
    bool wake = false;

    if (slow_tx_ns) {
        memset(&trace, 0, sizeof(trace));
        tr = &trace;
        tr->irqs_start = kstat_this_cpu->irqs_sum;
        tr->t[XT_START] = local_clock();
    }

    rcu_read_lock();

    /* Check if device is started */
    if (!ctx->started)
        goto drop;

    /*
     * Queue selection currently follows skb->queue_mapping.
     * Custom steering can be added later via ndo_select_queue or a
     * driver-specific hashing policy if needed.
     */
    queue = rattan_get_xmit_queue(ctx, skb);
    if (!queue)
        goto drop;

    /*
     * No reference on rings here: we are inside rcu_read_lock(), and
     * fop_release unpublishes the rings and waits for RCU before dropping
     * the queue's own reference. Each slot takes its own reference below.
     */
    rings = rcu_dereference(queue->rings);
    umem = rcu_dereference(ctx->umem);
    if (!rings || !umem)
        goto drop;
    XT(tr, XT_RESOLVE);

    /*
     * Handle GSO packets: segment them into individual MTU-sized SKBs.
     * This ensures each entry in the hash table is a standard packet,
     * which simplifies forwarding and allows per-packet processing.
     */
    if (skb_is_gso(skb)) {
        segs = skb_gso_segment(skb, 0);
        if (IS_ERR_OR_NULL(segs)) {
            dev_kfree_skb_any(skb);
            rattan_tx_dropped_inc(dev);
            goto out;
        }

        /* Free the original GSO SKB */
        consume_skb(skb);

        /* Process each segment */
        while (segs) {
            next = segs->next;
            segs->next = NULL;

            ret = rattan_xmit_one(segs, dev, queue, rings, umem, tr);
            if (tr)
                tr->nsegs++;
            if (ret < 0) {
                /* Drop this and remaining segments */
                dev_kfree_skb_any(segs);
                rattan_tx_dropped_inc(dev);
                while (next) {
                    segs = next;
                    next = segs->next;
                    dev_kfree_skb_any(segs);
                    rattan_tx_dropped_inc(dev);
                }
                goto out;
            }
            wake = true;
            segs = next;
        }
    } else {
        /* Non-GSO packet: process directly */
        ret = rattan_xmit_one(skb, dev, queue, rings, umem, tr);
        if (tr)
            tr->nsegs = 1;
        if (ret < 0) {
            dev_kfree_skb_any(skb);
            rattan_tx_dropped_inc(dev);
            goto out;
        }
        wake = true;
    }

out:
    rcu_read_unlock();
    if (wake) {
        rattan_rx_notify(queue, tr);
        XT(tr, XT_NOTIFY);
    }

    rattan_report_slow_tx(tr, queue, "xmit");
    return NETDEV_TX_OK;

drop:
    rcu_read_unlock();
    dev_kfree_skb_any(skb);
    rattan_tx_dropped_inc(dev);

    rattan_report_slow_tx(tr, queue, "drop");
    return NETDEV_TX_OK;
}

/*
 * Net device setup - called by alloc_netdev
 */
static void rattan_vnic_setup(struct net_device *dev) {
    ether_setup(dev);

    dev->netdev_ops = &rattan_netdev_ops;
#ifdef RATTAN_CORE_TSTATS
    /* Per-cpu tx/rx packet and byte counters, allocated by the core */
    dev->pcpu_stat_type = NETDEV_PCPU_STAT_TSTATS;
#endif

    /* Virtual device flags */
    dev->flags &= ~IFF_MULTICAST;

    /* use no queue */
    dev->priv_flags |= IFF_NO_QUEUE;

    pr_info("RVnic flags: 0x%x 0x%x\n", dev->flags, dev->priv_flags);

    /* Generate random MAC address */
    eth_hw_addr_random(dev);

    /* Standard MTU */
    dev->mtu = ETH_DATA_LEN;
    dev->min_mtu = 68;
    dev->max_mtu = 65535;

    /* GSO/GRO features - large packets get segmented in start_xmit */
    dev->features |=
        NETIF_F_GSO | NETIF_F_GRO | NETIF_F_SG | NETIF_F_HIGHDMA | NETIF_F_GSO_SOFTWARE;

    dev->hw_features |=
        NETIF_F_GSO | NETIF_F_GRO | NETIF_F_SG | NETIF_F_HIGHDMA | NETIF_F_GSO_SOFTWARE;

    /* No carrier until started */
    netif_carrier_off(dev);
}

/* ========== Cleanup Helpers ========== */

/*
 * UMEM cleanup helper - decrements refcount, frees if last user
 */
static void rattan_put_umem(struct rattan_umem *umem) {
    if (!umem)
        return;

    if (!refcount_dec_and_test(&umem->refcount))
        return; /* Still in use by other devices */

    /* Last user - actually free the UMEM */
    mutex_lock(&rattan_umems_lock);
    list_del(&umem->node);
    mutex_unlock(&rattan_umems_lock);

    rattan_umem_drain(umem);
    vfree(umem->pending);

    if (umem->mapped)
        vunmap(umem->mapped);

    if (umem->pages) {
        rattan_unpin_pages(umem->pages, umem->nr_pages);
        kvfree(umem->pages);
    }

    kfree(umem);
}

static struct rattan_queue *rattan_get_queue(struct rattan_vnic_ctx *ctx, u32 qid) {
    if (!ctx || qid >= ctx->num_queues)
        return NULL;

    return &ctx->queues[qid];
}

static struct rattan_queue *rattan_get_xmit_queue(struct rattan_vnic_ctx *ctx,
                                                  struct sk_buff *skb) {
    u16 qid;

    if (!ctx || !ctx->num_queues)
        return NULL;

    qid = skb_get_queue_mapping(skb);
    if (qid >= ctx->num_queues)
        qid %= ctx->num_queues;

    return &ctx->queues[qid];
}

static struct rattan_rings *rattan_get_rings(struct rattan_queue *queue) {
    struct rattan_rings *rings;

    if (!queue)
        return NULL;

    rings = rcu_dereference(queue->rings);
    if (rings)
        refcount_inc(&rings->refcount);

    return rings;
}

static void rattan_cleanup_rings(struct rattan_rings *rings) {
    if (!rings)
        return;

    might_sleep();

    if (rings->mapped)
        vunmap(rings->mapped);

    if (rings->pages) {
        rattan_unpin_pages(rings->pages, rings->nr_pages);
        kvfree(rings->pages);
    }

    // kfree(rings->recycle);
    kfree(rings);
}

static void rattan_cleanup_rings_work(struct work_struct *work) {
    rattan_cleanup_rings(container_of(work, struct rattan_rings, cleanup_work));
}

/*
 * Drop a reference on rings. Safe from any context: the last put only queues
 * the cleanup, which sleeps, to process context.
 */
static void rattan_put_rings(struct rattan_rings *rings) {
    if (!rings)
        return;

    if (refcount_dec_and_test(&rings->refcount))
        queue_work(rattan_cleanup_wq, &rings->cleanup_work);
}

/*
 * Push n chunk addresses to rings' COMP ring under one lock and one producer
 * update. Returns how many fit; the rest are dropped (warned and counted).
 */
static u32 rattan_complete_chunks(struct rattan_queue *queue, struct rattan_rings *rings,
                                  const u64 *addrs, u32 n) {
    u32 pushed;

    if (!rings || !n)
        return 0;
    /*
     * Concurrency model:
     *
     * This comp ring is used as MPSC (multi-producer, single-consumer):
     *
     *   Producers:
     *     - napi_poll() context (softirq)
     *     - process context (e.g., ioctl)
     *     -> may run concurrently on different CPUs
     *
     *   Consumer:
     *     - ndo_start_xmit()
     *     -> single-consumer per TX queue (no concurrent pop)
     *
     * Therefore, producers must be serialized.
     *
     * We use spin_lock_bh() here because:
     *   1. It provides mutual exclusion across CPUs (spinlock)
     *   2. It disables softirqs on the local CPU, preventing reentry from
     *      napi_poll() while running in process context
     *
     * Note:
     *   - We do NOT use spin_lock_irqsave() because there is no hard IRQ
     *     context accessing this ring (no real NIC / interrupt handler).
     *   - If this changes in the future (e.g., push from hard IRQ), this
     *     must be upgraded to spin_lock_irqsave().
     *
     * Also note:
     *   - The ring itself is implemented as SPSC (lockless) between
     *     producer and consumer, using acquire/release barriers.
     *   - This lock only serializes multiple producers (MPSC -> SPSC).
     */

    spin_lock_bh(&rings->comp_lock);
    pushed = comp_ring_push_many(rings->comp, addrs, n);
    spin_unlock_bh(&rings->comp_lock);

    if (queue) {
        RATTAN_DBG_ADD(queue, comp_ok, pushed);
        if (pushed < n)
            RATTAN_DBG_ADD(queue, comp_full, n - pushed);
    }

    if (pushed < n)
        pr_warn_ratelimited("rattan%d:q%u completion ring full, dropping %u chunks from 0x%llx\n",
                            queue ? queue->ctx->dev_id : -1, queue ? queue->qid : 0, n - pushed,
                            addrs[pushed]);

    return pushed;
}

static bool rattan_complete_chunk(struct rattan_queue *queue, struct rattan_rings *rings,
                                  u64 addr) {
    return rattan_complete_chunks(queue, rings, &addr, 1) == 1;
}

/*
 * Per-NAPI-poll completion batching.
 *
 * Every forwarded packet returns its chunk to the COMP ring of the queue that
 * supplied it (the owner, usually another device's queue on another CPU), and
 * drops the owner reference its slot held. Doing that per packet bounces the
 * owner's comp_lock/refcount line and COMP producer line between cores on
 * every packet. Instead, NAPI collects addresses per destination rings here
 * and pushes each group with one lock, one producer store and one
 * refcount_sub.
 *
 * Kept small: this lives on the NAPI stack frame, underneath
 * netif_receive_skb() and everything the stack does from there.
 */
#define RATTAN_COMP_GROUPS 2
#define RATTAN_COMP_BATCH 32

struct rattan_comp_batch {
    struct rattan_queue *queue; /* For statistics */
    struct {
        struct rattan_rings *rings;
        u32 n;    /* Addresses in addrs[] */
        u32 refs; /* Slot references on rings to drop at flush */
        u64 addrs[RATTAN_COMP_BATCH];
    } g[RATTAN_COMP_GROUPS];
};

static void rattan_comp_batch_init(struct rattan_comp_batch *b, struct rattan_queue *queue) {
    int i;

    b->queue = queue;
    for (i = 0; i < RATTAN_COMP_GROUPS; i++) {
        b->g[i].rings = NULL;
        b->g[i].n = 0;
        b->g[i].refs = 0;
    }
}

static void rattan_comp_batch_flush_group(struct rattan_comp_batch *b, int i) {
    struct rattan_rings *rings = b->g[i].rings;

    rattan_complete_chunks(b->queue, rings, b->g[i].addrs, b->g[i].n);
    if (b->g[i].refs && refcount_sub_and_test(b->g[i].refs, &rings->refcount))
        queue_work(rattan_cleanup_wq, &rings->cleanup_work);

    b->g[i].rings = NULL;
    b->g[i].n = 0;
    b->g[i].refs = 0;
}

static void rattan_comp_batch_flush(struct rattan_comp_batch *b) {
    int i;

    for (i = 0; i < RATTAN_COMP_GROUPS; i++)
        if (b->g[i].rings)
            rattan_comp_batch_flush_group(b, i);
}

/* Addresses queued for rings but not yet on its COMP ring */
static u32 rattan_comp_batch_pending(struct rattan_comp_batch *b, struct rattan_rings *rings) {
    int i;

    for (i = 0; i < RATTAN_COMP_GROUPS; i++)
        if (b->g[i].rings == rings)
            return b->g[i].n;
    return 0;
}

/*
 * Queue addr for rings' COMP ring. With put_ref, the batch also takes over
 * one reference on rings (the one a slot held) and drops it at flush; the
 * caller must not touch rings after that unless it holds its own.
 */
static void rattan_comp_batch_add(struct rattan_comp_batch *b, struct rattan_rings *rings,
                                  u64 addr, bool put_ref) {
    int i, slot = -1;

    for (i = 0; i < RATTAN_COMP_GROUPS; i++) {
        if (b->g[i].rings == rings) {
            slot = i;
            break;
        }
        if (!b->g[i].rings && slot < 0)
            slot = i;
    }

    if (slot < 0) {
        /* Every group holds other rings: evict the last one */
        slot = RATTAN_COMP_GROUPS - 1;
        rattan_comp_batch_flush_group(b, slot);
    } else if (b->g[slot].n == RATTAN_COMP_BATCH) {
        rattan_comp_batch_flush_group(b, slot);
    }

    b->g[slot].rings = rings;
    b->g[slot].addrs[b->g[slot].n++] = addr;
    if (put_ref)
        b->g[slot].refs++;
}

/* ========== Ring Helper Functions ========== */

/* Check if ring is empty (producer == consumer) */
static inline bool ring_is_empty(struct rattan_ring *ring) {
    return READ_ONCE(ring->producer) == READ_ONCE(ring->consumer);
}

/* Check if ring is full */
static inline bool ring_is_full(struct rattan_ring *ring) {
    u32 prod = READ_ONCE(ring->producer);
    u32 cons = READ_ONCE(ring->consumer);
    return ((prod - cons) >= RATTAN_RING_SIZE);
}

/* Get number of entries in ring */
static inline u32 ring_count(struct rattan_ring *ring) {
    u32 prod = READ_ONCE(ring->producer);
    u32 cons = READ_ONCE(ring->consumer);
    return prod - cons;
}

/*
 * Pop chunk address from FILL ring (kernel is consumer)
 * Returns true on success, false if ring empty
 */
static bool fill_ring_pop(struct rattan_fill_ring *fill, u64 *addr) {
    u32 cons, prod;

    prod = READ_ONCE(fill->ring.producer);
    cons = READ_ONCE(fill->ring.consumer);

    /* Memory barrier to ensure we see updated producer */
    smp_rmb();

    if (cons == prod)
        return false; /* Ring empty */

    *addr = fill->addrs[cons & RATTAN_RING_MASK];

    /* Memory barrier before updating consumer */
    smp_wmb();
    WRITE_ONCE(fill->ring.consumer, cons + 1);

    return true;
}

/*
 * Get a chunk address, trying COMP ring first, then FILL ring
 * The recycle ring is protected by rings->recycle_lock.
 * Returns true on success, false if both rings empty
 */
static bool try_get_free_addr(struct rattan_queue *queue, struct rattan_rings *rings,
                              struct rattan_umem *umem, u64 *addr,
                              struct rattan_xmit_trace *tr) {
    bool ok;

    // As long as we only pop in `rattan_xmit_one`, we do not need to lock
    // spin_lock_bh(&rings->comp_lock);
    ok = comp_ring_pop(rings->comp, addr);
    // spin_unlock_bh(&rings->comp_lock);

    if (rattan_recycle_drop_token(umem, rings->drop)) {
        RATTAN_DBG_INC(queue, recycle_token);
        if (tr)
            tr->recycled = true;
    }

    if (ok)
        return true;

    return fill_ring_pop(rings->fill, addr);
}

/*
 * Check if RX ring has space for at least one entry
 * Used for backpressure - check before consuming from FILL ring
 */
static bool rx_ring_has_space(struct rattan_rx_ring *rx) {
    u32 cons, prod;

    prod = READ_ONCE(rx->ring.producer);
    cons = READ_ONCE(rx->ring.consumer);

    return (prod - cons) < RATTAN_RING_SIZE;
}

/*
 * Push descriptor to RX ring (kernel is producer)
 * Returns:
 *   < 0: error (ring full)
 *   0: success
 */
static int rx_ring_push(struct rattan_rx_ring *rx, struct rattan_desc *desc) {
    u32 cons, prod;

    prod = READ_ONCE(rx->ring.producer);
    cons = READ_ONCE(rx->ring.consumer);

    if ((prod - cons) >= RATTAN_RING_SIZE)
        return -ENOSPC; /* Ring full */

    rx->descs[prod & RATTAN_RING_MASK] = *desc;

    /* Memory barrier before updating producer */
    smp_wmb();
    WRITE_ONCE(rx->ring.producer, prod + 1);

    return 0;
}

/*
 * Free entries in the COMP ring
 * Used for backpressure - check before consuming from TX ring
 */
static u32 comp_ring_free(struct rattan_comp_ring *comp) {
    u32 cons, prod;

    prod = READ_ONCE(comp->ring.producer);
    cons = READ_ONCE(comp->ring.consumer);

    return RATTAN_RING_SIZE - (prod - cons);
}

/*
 * Push up to n chunk addresses to the COMP ring with a single producer
 * update. Returns how many fit.
 */
static u32 comp_ring_push_many(struct rattan_comp_ring *comp, const u64 *addrs, u32 n) {
    u32 cons, prod, i;

    prod = READ_ONCE(comp->ring.producer);
    // Make sure that we see the updated value
    cons = smp_load_acquire(&comp->ring.consumer);

    n = min_t(u32, n, RATTAN_RING_SIZE - (prod - cons));
    for (i = 0; i < n; i++)
        comp->addrs[(prod + i) & RATTAN_RING_MASK] = addrs[i];

    if (n)
        smp_store_release(&comp->ring.producer, prod + n);

    return n;
}

/*
 * Pop chunk address from COMP ring
 * Returns true on success, false if ring empty
 */
static bool comp_ring_pop(struct rattan_comp_ring *comp, u64 *addr) {
    u32 cons, prod;

    cons = READ_ONCE(comp->ring.consumer);
    prod = smp_load_acquire(&comp->ring.producer);

    if (cons == prod)
        return false;

    *addr = comp->addrs[cons & RATTAN_RING_MASK];

    smp_store_release(&comp->ring.consumer, cons + 1);

    return true;
}

/*
 * Pop token from DROP ring, and release them.
 */
static bool rattan_recycle_drop_token(struct rattan_umem *umem, struct rattan_drop_ring *drop) {
    u32 cons, prod;
    u64 token;

    cons = READ_ONCE(drop->ring.consumer);
    prod = smp_load_acquire(&drop->ring.producer);

    if (cons == prod)
        return false;

    token = drop->tokens[cons & RATTAN_RING_MASK];

    smp_store_release(&drop->ring.consumer, cons + 1);

    return release_token(umem, token);
}

/*
 * Peek at next descriptor in TX ring without consuming
 * Returns true if descriptor available, false if ring empty
 */
static bool tx_ring_peek(struct rattan_tx_ring *tx, struct rattan_desc *desc) {
    u32 cons, prod;

    prod = READ_ONCE(tx->ring.producer);
    cons = READ_ONCE(tx->ring.consumer);

    /* Memory barrier to ensure we see updated producer */
    smp_rmb();

    if (cons == prod)
        return false; /* Ring empty */

    *desc = tx->descs[cons & RATTAN_RING_MASK];
    return true;
}

/*
 * Advance TX ring consumer after peeking
 * Call this after tx_ring_peek() to consume the descriptor
 */
static void tx_ring_advance(struct rattan_tx_ring *tx) {
    u32 cons = READ_ONCE(tx->ring.consumer);

    /* Memory barrier before updating consumer */
    smp_wmb();
    WRITE_ONCE(tx->ring.consumer, cons + 1);
}

/* ========== Token/SKB Management ========== */

static long rattan_pending_count(void) {
    long sum = 0;
    int cpu;

    for_each_possible_cpu(cpu)
        sum += per_cpu(rattan_pending_tokens, cpu);
    return sum;
}

static inline u64 rattan_chunk_addr(struct rattan_umem *umem, u32 idx) {
    return (u64)idx << umem->chunk_shift;
}

/*
 * The UMEM of a device, from an ioctl on its fd. ctx->umem only goes from NULL
 * to a UMEM in ioctls and back to NULL in fop_release, which cannot run while
 * an ioctl on the same file is in flight, so no RCU read section is needed.
 */
static inline struct rattan_umem *rattan_ctx_umem(struct rattan_vnic_ctx *ctx) {
    return rcu_dereference_check(ctx->umem, 1);
}

/*
 * Bind skb to the (just obtained) chunk idx: FREE(g) -> BUSY(g) -> LIVE(g+1).
 * The slot takes over the caller's reference on rings.
 *
 * Returns the new token, or 0 if the slot is not free, i.e. userspace handed
 * out a chunk that is still bound to a live token.
 */
static u64 rattan_slot_insert(struct rattan_umem *umem, u32 idx, struct sk_buff *skb,
                              struct rattan_rings *rings, u16 qid) {
    struct rattan_pending *p;
    u64 old;
    u32 gen;

    /* Callers validate the chunk first; this guards the array regardless */
    if (WARN_ON_ONCE(idx >= umem->nr_chunks))
        return 0;

    p = &umem->pending[idx];
    old = READ_ONCE(p->state);
    if ((old & RATTAN_SLOT_TAG_MASK) != RATTAN_SLOT_FREE ||
        cmpxchg(&p->state, old, old | RATTAN_SLOT_BUSY) != old)
        return 0;

    gen = RATTAN_SLOT_GEN(old) + 1;
    if (unlikely(!gen))
        gen = 1;

    p->skb = skb;
    p->rings = rings;
    p->stamp = (u32)jiffies;
    p->qid = qid;

    /* Publish the fields together with LIVE */
    smp_store_release(&p->state, RATTAN_SLOT_STATE(gen, RATTAN_SLOT_LIVE));
    this_cpu_inc(rattan_pending_tokens);

    return RATTAN_TOKEN(idx, gen);
}

/*
 * Claim the slot of a live token: LIVE(gen) -> BUSY(gen). The one cmpxchg
 * checks liveness and generation together, so a stale, replayed or
 * concurrently claimed token misses. Returns NULL on a miss.
 *
 * The caller must end the claim with rattan_slot_take() or
 * rattan_slot_unclaim() without sleeping; process-context callers disable BH
 * so that NAPI on the same CPU never finds the slot BUSY for long.
 */
static struct rattan_pending *rattan_slot_claim(struct rattan_umem *umem, u64 token) {
    u32 idx = RATTAN_TOKEN_IDX(token);
    u32 gen = RATTAN_TOKEN_GEN(token);
    u64 live = RATTAN_SLOT_STATE(gen, RATTAN_SLOT_LIVE);
    struct rattan_pending *p;

    if (unlikely(idx >= umem->nr_chunks))
        return NULL;

    p = &umem->pending[idx];
    if (cmpxchg(&p->state, live, RATTAN_SLOT_STATE(gen, RATTAN_SLOT_BUSY)) != live)
        return NULL;

    return p;
}

/* BUSY(gen) -> LIVE(gen): hand a claimed slot back unchanged */
static inline void rattan_slot_unclaim(struct rattan_pending *p, u64 token) {
    smp_store_release(&p->state, RATTAN_SLOT_STATE(RATTAN_TOKEN_GEN(token), RATTAN_SLOT_LIVE));
}

/*
 * BUSY(gen) -> FREE(gen): empty a claimed slot. The caller inherits the skb
 * and the rings reference, and still owes the chunk a completion.
 */
static struct sk_buff *rattan_slot_take(struct rattan_pending *p, u64 token,
                                        struct rattan_rings **rings) {
    struct sk_buff *skb = p->skb;

    *rings = p->rings;
    p->skb = NULL;
    p->rings = NULL;

    /* The reads above must complete before the slot can be reused */
    smp_store_release(&p->state, RATTAN_SLOT_STATE(RATTAN_TOKEN_GEN(token), RATTAN_SLOT_FREE));
    this_cpu_dec(rattan_pending_tokens);

    return skb;
}

/* Claim and empty a token's slot in one go. Returns NULL on a miss. */
static struct sk_buff *rattan_token_take(struct rattan_umem *umem, u64 token,
                                         struct rattan_rings **rings) {
    struct rattan_pending *p = rattan_slot_claim(umem, token);

    return p ? rattan_slot_take(p, token, rings) : NULL;
}

/*
 * Finish releasing a taken token: return its chunk to the owning COMP ring and
 * free the SKB.
 */
static void rattan_token_drop(struct rattan_umem *umem, u64 token, struct sk_buff *skb,
                              struct rattan_rings *rings) {
    /* The queue may have been released here, if the fd has been closed,
     * since the pointer to the queue is used for statistics and debugging only,
     * we pass NULL to avoid possible use-after-free.
     */
    rattan_complete_chunk(NULL, rings, rattan_chunk_addr(umem, RATTAN_TOKEN_IDX(token)));
    rattan_put_rings(rings);
    dev_kfree_skb_any(skb);
}

/*
 * Release token - free the SKB associated with a token and return its chunk
 * to the owning COMP ring. Softirq context (the DROP ring); process context
 * uses rattan_release_token_process().
 */
static bool release_token(struct rattan_umem *umem, u64 token) {
    struct rattan_rings *rings;
    struct sk_buff *skb;

    skb = rattan_token_take(umem, token, &rings);
    if (!skb)
        return false;

    rattan_token_drop(umem, token, skb, rings);
    return true;
}

/*
 * release_token() for process context. BH is disabled only while the slot is
 * claimed, so NAPI on this CPU never finds it BUSY for longer than that.
 */
static bool rattan_release_token_process(struct rattan_umem *umem, u64 token) {
    struct rattan_rings *rings;
    struct sk_buff *skb;

    local_bh_disable();
    skb = rattan_token_take(umem, token, &rings);
    local_bh_enable();
    if (!skb)
        return false;

    rattan_token_drop(umem, token, skb, rings);
    return true;
}

/*
 * Free whatever is still pending on a UMEM that has no users left. Nothing
 * can race with this: every device that used the UMEM has unpublished it and
 * waited for RCU, and it is off rattan_umems so GC cannot see it. The chunks
 * are not completed, since the UMEM itself is going away.
 */
static void rattan_umem_drain(struct rattan_umem *umem) {
    u32 idx, leaked = 0;

    for (idx = 0; idx < umem->nr_chunks; idx++) {
        struct rattan_pending *p = &umem->pending[idx];

        if ((p->state & RATTAN_SLOT_TAG_MASK) != RATTAN_SLOT_LIVE)
            continue;

        rattan_put_rings(p->rings);
        dev_kfree_skb_any(p->skb);
        this_cpu_dec(rattan_pending_tokens);
        leaked++;
    }

    if (leaked)
        pr_info("released %u pending SKBs with their UMEM\n", leaked);
}

/*
 * Read payload from tracked SKB
 * The token resolves through this device's UMEM, so it must have been minted
 * on a device sharing it.
 */
static int rattan_read_payload(struct rattan_vnic_ctx *ctx,
                               struct rattan_payload_req __user *argp) {
    struct rattan_payload_req req;
    struct rattan_payload_resp resp;
    struct rattan_umem *umem;
    struct rattan_pending *p;
    struct sk_buff *skb;
    void *kbuf;
    u32 copy_len;
    int err;

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    umem = rattan_ctx_umem(ctx);
    if (!umem)
        return -ENOENT;

    local_bh_disable();
    p = rattan_slot_claim(umem, req.token);
    if (!p) {
        local_bh_enable();
        return -ENOENT;
    }

    skb = p->skb;
    resp.total_len = skb->len;

    /* Calculate how much to copy */
    if (req.offset >= skb->len) {
        rattan_slot_unclaim(p, req.token);
        local_bh_enable();
        copy_len = 0;
        resp.len = 0;
        goto write_resp;
    }

    copy_len = min_t(u32, req.len, skb->len - req.offset);
    resp.len = copy_len;

    /* Take reference and give the slot back */
    skb_get(skb);
    rattan_slot_unclaim(p, req.token);
    local_bh_enable();

    /* Allocate kernel buffer for copy */
    kbuf = kmalloc(copy_len, GFP_KERNEL);
    if (!kbuf) {
        kfree_skb(skb);
        return -ENOMEM;
    }

    /* Copy from SKB to kernel buffer */
    err = skb_copy_bits(skb, req.offset, kbuf, copy_len);
    kfree_skb(skb);

    if (err) {
        kfree(kbuf);
        return err;
    }

    /* Copy to userspace */
    if (copy_to_user((void __user *)req.buf, kbuf, copy_len)) {
        kfree(kbuf);
        return -EFAULT;
    }

    kfree(kbuf);

write_resp:
    /* Write response */
    if (copy_to_user(argp, &resp, sizeof(resp)))
        return -EFAULT;

    return 0;
}

/*
 * Release token, ioctl handler
 */
static int rattan_release_token(struct rattan_vnic_ctx *ctx,
                                struct rattan_release_req __user *argp) {
    struct rattan_release_req req;
    struct rattan_umem *umem;
    bool ok;

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    umem = rattan_ctx_umem(ctx);
    if (!umem)
        return -ENOENT;

    ok = rattan_release_token_process(umem, req.token);

    return ok ? 0 : -ENOENT;
}

/*
 * Clone a token - bind a copy of its SKB to a chunk supplied by userspace
 */
static int rattan_clone_token(struct rattan_vnic_ctx *ctx, struct rattan_clone_req __user *argp) {
    struct rattan_clone_req req;
    struct rattan_clone_resp resp;
    struct rattan_umem *umem;
    struct rattan_queue *queue;
    struct rattan_rings *rings, *old_rings;
    struct rattan_pending *p;
    struct sk_buff *new_skb, *old_skb;
    u64 new_token;
    int err;

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    umem = rattan_ctx_umem(ctx);
    queue = rattan_get_queue(ctx, req.queue_id);
    if (!umem || !queue)
        return -EINVAL;

    if (!rattan_chunk_valid(umem, req.chunk_addr))
        return -EINVAL;

    /* The new slot owns this reference */
    rcu_read_lock();
    rings = rattan_get_rings(queue);
    rcu_read_unlock();
    if (!rings)
        return -EINVAL;

    local_bh_disable();
    p = rattan_slot_claim(umem, req.token);
    if (!p) {
        err = -ENOENT;
        goto out_unlock;
    }

    /* Copy the SKB (not clone - we need independent data for modifications) */
    new_skb = skb_copy(p->skb, GFP_ATOMIC);
    rattan_slot_unclaim(p, req.token);
    if (!new_skb) {
        err = -ENOMEM;
        goto out_unlock;
    }

    new_token = rattan_slot_insert(umem, req.chunk_addr >> umem->chunk_shift, new_skb, rings,
                                   queue->qid);
    if (!new_token) {
        kfree_skb(new_skb);
        err = -EBUSY;
        goto out_unlock;
    }
    local_bh_enable();

    resp.new_token = new_token;
    if (copy_to_user(argp, &resp, sizeof(resp))) {
        /*
         * Userspace never learns the token, so undo the insert. The chunk
         * stays with userspace, which still owns it as far as it knows.
         */
        local_bh_disable();
        old_skb = rattan_token_take(umem, new_token, &old_rings);
        local_bh_enable();
        if (old_skb) {
            rattan_put_rings(old_rings);
            kfree_skb(old_skb);
        }
        return -EFAULT;
    }

    return 0;

out_unlock:
    local_bh_enable();
    rattan_put_rings(rings);
    return err;
}

static int rattan_device_id(struct rattan_vnic_ctx *ctx, void __user *argp) {
    int dev_id = ctx->dev_id;
    if (copy_to_user(argp, &dev_id, sizeof(dev_id))) {
        return -EFAULT;
    }
    return 0;
}

static int rattan_napi_poll(struct napi_struct *napi, int budget) {
    struct rattan_queue *queue = container_of(napi, struct rattan_queue, napi);
    struct rattan_vnic_ctx *ctx = queue->ctx;
    struct rattan_rings *rings;
    struct rattan_umem *umem;
    struct net_device *dev = ctx->netdev;
    struct rattan_desc desc;
    struct rattan_pending *p;
    struct rattan_rings *owner;
    struct sk_buff *skb;
    u64 chunk_addr;
    int processed = 0;
    int pkt_len;
    struct rattan_comp_batch comp;
    struct rattan_napi_trace trace;
    struct rattan_napi_trace *tr = NULL;

    RATTAN_DBG_INC(queue, napi_polls);
    rattan_debug_dump_queue_stats(queue);

    rcu_read_lock();

    if (!ctx->started) {
        rcu_read_unlock();
        goto complete;
    }

    rings = rcu_dereference(queue->rings);
    umem = rcu_dereference(ctx->umem);
    if (!rings || !umem) {
        rcu_read_unlock();
        goto complete;
    }

    rattan_comp_batch_init(&comp, queue);

    while (processed < budget) {
        if (slow_rx_ns) {
            tr = &trace;
            rattan_napi_trace_begin(tr);
        }

        /*
         * Backpressure: queue-local COMP ring must have space, counting what
         * this poll has batched for it but not pushed yet.
         */
        if (comp_ring_free(rings->comp) <= rattan_comp_batch_pending(&comp, rings)) {
            RATTAN_DBG_INC(queue, napi_comp_full);
            pr_warn_ratelimited("rattan%d:q%u NAPI blocked: COMP ring full\n", ctx->dev_id,
                                queue->qid);
            break;
        }

        /* Peek at next TX descriptor */
        if (!tx_ring_peek(rings->tx, &desc)) {
            RATTAN_DBG_INC(queue, napi_tx_empty);
            break; /* TX ring empty */
        }
        NT(tr, NT_PEEK);

        /* Claim the token's pending slot */
        p = rattan_slot_claim(umem, desc.token);
        if (!p) {
            /* Token not found - consume descriptor, return TX chunk */
            tx_ring_advance(rings->tx);
            RATTAN_DBG_INC(queue, napi_token_miss);
            rattan_comp_batch_add(&comp, rings, desc.addr, false);
            pr_warn_ratelimited("rattan%d:q%u NAPI token miss: 0x%llx\n", ctx->dev_id, queue->qid,
                                desc.token);
            processed++;
            NT(tr, NT_RELEASE);
            rattan_report_slow_rx(tr, queue, "miss");
            continue;
        }
        NT(tr, NT_LOOKUP);

        /* Consume descriptor and empty the slot */
        tx_ring_advance(rings->tx);
        skb = rattan_slot_take(p, desc.token, &owner);
        NT(tr, NT_UNLINK);

        /*
         * Copy modified header back to SKB. Compiled out while
         * RATTAN_HEADER_SIZE is 0, and not correct as it stands. Before
         * raising it:
         *   - skb_ensure_writable(skb, copy_len) first: TCP transmits clones,
         *     so writing in place corrupts the sender's retransmit copy (and
         *     with sendfile, page-cache pages);
         *   - settle a checksum contract (userspace patches incrementally,
         *     RFC 1624, or the kernel fixes them up here);
         *   - the header size is part of the ABI: it is overwritten in place
         *     at offset 0.
         */
        if (RATTAN_HEADER_SIZE && desc.len > 0) {
            if (rattan_chunk_valid(umem, desc.addr) &&
                umem->headroom + desc.len <= umem->chunk_size) {
                void *header_ptr = (u8 *)umem->mapped + desc.addr + umem->headroom;
                u32 copy_len = min3(desc.len, skb->len, (u32)RATTAN_HEADER_SIZE);
                skb_store_bits(skb, 0, header_ptr, copy_len);
            }
        }

        /* Record length and prepare SKB for injection */
        pkt_len = skb->len;
        skb->dev = dev;
        skb_record_rx_queue(skb, queue->qid);
        skb->protocol = eth_type_trans(skb, dev);
        NT(tr, NT_PREP);

        /*
         * Use netif_receive_skb() instead of netif_rx() since we're
         * already in NAPI/softirq context on the target CPU.
         */
        if (netif_receive_skb(skb) == NET_RX_SUCCESS) {
            RATTAN_DBG_INC(queue, napi_rx_ok);
            dev_sw_netstats_rx_add(dev, pkt_len);
        } else {
            RATTAN_DBG_INC(queue, napi_rx_drop);
            rattan_rx_dropped_inc(dev);
        }
        NT(tr, NT_STACK);

        /*
         * The token names the original chunk; desc.addr is whichever chunk
         * userspace sent back, which may differ. Both go home.
         */
        chunk_addr = rattan_chunk_addr(umem, RATTAN_TOKEN_IDX(desc.token));
        if (desc.addr != chunk_addr)
            rattan_comp_batch_add(&comp, rings, desc.addr, false);
        /* Hands the slot's owner reference to the batch */
        rattan_comp_batch_add(&comp, owner, chunk_addr, true);
        processed++;
        NT(tr, NT_RELEASE);
        rattan_report_slow_rx(tr, queue, "ok");
    }

    /* Before rcu_read_unlock(): groups for rings hold no reference */
    rattan_comp_batch_flush(&comp);
    rcu_read_unlock();

    if (processed < budget) {
    complete:
        /* All work done - exit polling mode */
        RATTAN_DBG_INC(queue, napi_complete);
        napi_complete_done(napi, processed);
        atomic_set(&queue->napi_scheduled, 0);
    } else {
        RATTAN_DBG_INC(queue, napi_budget_stop);
    }

    return processed;
}

/*
 * IPI callback to schedule NAPI on target CPU
 */
static void rattan_schedule_napi_ipi(void *data) {
    struct rattan_queue *queue = data;

    if (napi_schedule_prep(&queue->napi))
        __napi_schedule(&queue->napi);
}

/*
 * Inject packets into kernel receive path via NAPI
 *
 * Always uses NAPI for packet injection. If napi_cpu is not set (-1),
 * schedules NAPI on the current CPU. Otherwise schedules on the
 * configured target CPU via IPI if needed.
 */
static int rattan_kick_rx(struct rattan_queue *queue) {
    int target_cpu;
    int current_cpu;

    if (!queue || !queue->ctx->started)
        return -EINVAL;

    RATTAN_DBG_INC(queue, kick_rx);
    rattan_debug_dump_queue_stats(queue);

    current_cpu = get_cpu();
    target_cpu = READ_ONCE(queue->napi_cpu);

    /* If napi_cpu not configured, use current CPU */
    if (target_cpu < 0)
        target_cpu = current_cpu;

    /* Validate target CPU is still online, fallback to current */
    if (!cpu_online(target_cpu))
        target_cpu = current_cpu;

    if (target_cpu == current_cpu) {
        /* Target is current CPU - schedule directly */
        if (napi_schedule_prep(&queue->napi))
            __napi_schedule(&queue->napi);
        put_cpu();
    } else {
        put_cpu();

        /*
         * Target is remote CPU - send IPI.
         * Use napi_scheduled to avoid unnecessary IPIs when NAPI
         * is already scheduled. The IPI callback will do its own
         * napi_schedule_prep() check, but avoiding the IPI is cheaper.
         *
         * Per-queue CPU affinity can be customized further later if
         * custom steering is introduced.
         */
        if (atomic_cmpxchg(&queue->napi_scheduled, 0, 1) == 0) {
            RATTAN_DBG_INC(queue, kick_rx_remote);
            smp_call_function_single(target_cpu, rattan_schedule_napi_ipi, queue, 0);
        }
    }

    return 0;
}

/*
 * Internal GC helper - releases tokens older than timeout_ms on every UMEM
 *
 * A linear scan of each UMEM's pending slots. Each release is an ordinary
 * claim, so a token that is concurrently transmitted or released is simply
 * skipped (or makes the other side miss), never freed twice.
 *
 * Returns number of tokens collected.
 */
static u32 rattan_do_gc(u32 timeout_ms) {
    u32 timeout = msecs_to_jiffies(timeout_ms);
    struct rattan_umem *umem;
    u32 collected = 0;
    u32 idx;

    mutex_lock(&rattan_umems_lock);
    list_for_each_entry(umem, &rattan_umems, node) {
        for (idx = 0; idx < umem->nr_chunks; idx++) {
            struct rattan_pending *p = &umem->pending[idx];
            u64 state = READ_ONCE(p->state);

            if ((state & RATTAN_SLOT_TAG_MASK) != RATTAN_SLOT_LIVE)
                continue;
            if ((u32)jiffies - READ_ONCE(p->stamp) < timeout)
                continue;

            if (rattan_release_token_process(umem, RATTAN_TOKEN(idx, RATTAN_SLOT_GEN(state))))
                collected++;

            if ((idx & 4095) == 0)
                cond_resched();
        }
    }
    mutex_unlock(&rattan_umems_lock);

    return collected;
}

/*
 * Periodic GC work function
 *
 * Runs every RATTAN_GC_INTERVAL_MS (10 minutes) to clean up orphaned tokens.
 * This handles cases where userspace dropped packets without forwarding them.
 */
static void rattan_gc_work_fn(struct work_struct *work) {
    u32 collected;

    collected = rattan_do_gc(RATTAN_TOKEN_TIMEOUT_MS);

    if (collected > 0)
        pr_info("periodic GC: collected %u orphaned tokens, %ld remaining\n", collected,
                rattan_pending_count());

    /* Reschedule for next interval */
    schedule_delayed_work(&gc_work, msecs_to_jiffies(RATTAN_GC_INTERVAL_MS));
}

/* ========== IOCTL Handlers ========== */

/*
 * UMEM registration
 */
static int rattan_reg_umem(struct rattan_vnic_ctx *ctx, struct rattan_umem_reg __user *argp) {
    struct rattan_umem_reg params;
    struct rattan_umem *umem;
    unsigned long nr_pages;
    int ret;

    if (rcu_access_pointer(ctx->umem))
        return -EBUSY;

    if (copy_from_user(&params, argp, sizeof(params)))
        return -EFAULT;

    /* Validate parameters */
    if (params.len > RATTAN_MAX_UMEM_SIZE) {
        pr_warn("UMEM len %llu exceeds max %llu\n", params.len, (u64)RATTAN_MAX_UMEM_SIZE);
        return -EINVAL;
    }

    if (params.chunk_size < RATTAN_MIN_CHUNK_SIZE || params.chunk_size > RATTAN_MAX_CHUNK_SIZE)
        return -EINVAL;

    /* chunk_size must be power of 2 for efficient address validation */
    if (!is_power_of_2(params.chunk_size)) {
        pr_warn("chunk_size %u must be power of 2\n", params.chunk_size);
        return -EINVAL;
    }

    /* headroom + max header size must fit in chunk */
    if (params.headroom + RATTAN_HEADER_SIZE > params.chunk_size) {
        pr_warn("headroom %u + header %u > chunk_size %u\n", params.headroom, RATTAN_HEADER_SIZE,
                params.chunk_size);
        return -EINVAL;
    }

    if (params.len < params.chunk_size)
        return -EINVAL;

    /* UMEM length must be multiple of chunk_size (no partial chunks) */
    if (params.len % params.chunk_size != 0) {
        pr_warn("UMEM len %llu must be multiple of chunk_size %u\n", params.len, params.chunk_size);
        return -EINVAL;
    }

    /* One pending slot per chunk, so the chunk count bounds its memory */
    if (params.len / params.chunk_size > RATTAN_MAX_CHUNKS) {
        pr_warn("UMEM has %llu chunks, max %u\n", params.len / params.chunk_size,
                RATTAN_MAX_CHUNKS);
        return -EINVAL;
    }

    /* Userspace address must be page-aligned for correct vmap mapping */
    if (params.addr & (PAGE_SIZE - 1)) {
        pr_warn("UMEM addr 0x%llx not page-aligned\n", params.addr);
        return -EINVAL;
    }

    umem = kzalloc(sizeof(*umem), GFP_KERNEL);
    if (!umem)
        return -ENOMEM;

    umem->len = params.len;
    umem->chunk_size = params.chunk_size;
    umem->chunk_shift = ilog2(params.chunk_size);
    umem->headroom = params.headroom;
    umem->nr_chunks = params.len / params.chunk_size;
    INIT_LIST_HEAD(&umem->node);

    umem->pending = vzalloc(array_size(umem->nr_chunks, sizeof(*umem->pending)));
    if (!umem->pending) {
        ret = -ENOMEM;
        goto err_free;
    }

    nr_pages = (params.len + PAGE_SIZE - 1) / PAGE_SIZE;
    umem->pages = kvmalloc_array(nr_pages, sizeof(struct page *), GFP_KERNEL);
    if (!umem->pages) {
        ret = -ENOMEM;
        goto err_free;
    }

    ret = rattan_pin_pages(params.addr, nr_pages, FOLL_WRITE, umem->pages);
    if (ret < 0)
        goto err_free;

    if (ret != nr_pages) {
        rattan_unpin_pages(umem->pages, ret);
        ret = -ENOMEM;
        goto err_free;
    }

    umem->nr_pages = nr_pages;

    umem->mapped = vmap(umem->pages, nr_pages, VM_MAP, PAGE_KERNEL);
    if (!umem->mapped) {
        rattan_unpin_pages(umem->pages, nr_pages);
        ret = -ENOMEM;
        goto err_free;
    }

    /* Initialize refcount to 1 (this device owns it) */
    refcount_set(&umem->refcount, 1);

    mutex_lock(&rattan_umems_lock);
    list_add(&umem->node, &rattan_umems);
    mutex_unlock(&rattan_umems_lock);

    rcu_assign_pointer(ctx->umem, umem);
    pr_info("rattan%d: UMEM registered: %u chunks of %u bytes (headroom=%u)\n", ctx->dev_id,
            umem->nr_chunks, umem->chunk_size, umem->headroom);

    return 0;

err_free:
    kvfree(umem->pages);
    vfree(umem->pending);
    kfree(umem);
    return ret;
}

/*
 * Share UMEM from another device
 *
 * Instead of registering new UMEM, share an existing one from another device.
 * This enables zero-copy forwarding between devices.
 */
static int rattan_share_umem(struct rattan_vnic_ctx *ctx,
                             struct rattan_share_umem_req __user *argp) {
    struct rattan_share_umem_req req;
    struct rattan_vnic_ctx *source_ctx;
    struct rattan_umem *umem;
    struct file *source_file;

    if (rcu_access_pointer(ctx->umem))
        return -EBUSY; /* Already have UMEM */

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    /* Get the source file from fd */
    source_file = fget(req.source_fd);
    if (!source_file)
        return -EBADF;

    /* Verify it's a rattan-vnic file */
    if (source_file->f_op != &rattan_vnic_fops) {
        fput(source_file);
        return -EINVAL;
    }

    source_ctx = source_file->private_data;
    if (!source_ctx) {
        fput(source_file);
        return -EINVAL;
    }

    umem = rcu_access_pointer(source_ctx->umem);
    if (!umem) {
        fput(source_file);
        return -EINVAL; /* Source has no UMEM */
    }

    /* Increment refcount */
    refcount_inc(&umem->refcount);

    rcu_assign_pointer(ctx->umem, umem);
    fput(source_file);

    pr_info("rattan%d: sharing UMEM from rattan%d (refcount=%u)\n", ctx->dev_id, source_ctx->dev_id,
            refcount_read(&umem->refcount));

    return 0;
}

/*
 * Rings registration
 */
static int rattan_reg_rings_queue(struct rattan_vnic_ctx *ctx, u32 qid, u64 addr, u64 len) {
    struct rattan_queue *queue;
    struct rattan_rings *rings;
    unsigned long nr_pages;
    int ret;

    queue = rattan_get_queue(ctx, qid);
    if (!queue)
        return -EINVAL;

    if (rcu_access_pointer(queue->rings))
        return -EBUSY;

    if (!rcu_access_pointer(ctx->umem))
        return -EINVAL; /* UMEM must be registered first */

    /* Validate size */
    if (len < RATTAN_RINGS_SIZE) {
        pr_warn("rings len %llu too small (need %zu)\n", len, (size_t)RATTAN_RINGS_SIZE);
        return -EINVAL;
    }

    /* Userspace address must be page-aligned for correct vmap mapping */
    if (addr & (PAGE_SIZE - 1)) {
        pr_warn("rings addr 0x%llx not page-aligned\n", addr);
        return -EINVAL;
    }

    rings = kzalloc(sizeof(*rings), GFP_KERNEL);
    if (!rings)
        return -ENOMEM;

    refcount_set(&rings->refcount, 1);
    spin_lock_init(&rings->comp_lock);
    INIT_WORK(&rings->cleanup_work, rattan_cleanup_rings_work);
    rings->len = len;

    nr_pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    rings->pages = kvmalloc_array(nr_pages, sizeof(struct page *), GFP_KERNEL);
    if (!rings->pages) {
        kfree(rings);
        return -ENOMEM;
    }

    ret = rattan_pin_pages(addr, nr_pages, FOLL_WRITE, rings->pages);
    if (ret < 0) {
        kvfree(rings->pages);
        kfree(rings);
        return ret;
    }

    if (ret != nr_pages) {
        rattan_unpin_pages(rings->pages, ret);
        kvfree(rings->pages);
        kfree(rings);
        return -ENOMEM;
    }

    rings->nr_pages = nr_pages;

    rings->mapped = vmap(rings->pages, nr_pages, VM_MAP, PAGE_KERNEL);
    if (!rings->mapped) {
        rattan_unpin_pages(rings->pages, nr_pages);
        kvfree(rings->pages);
        kfree(rings);
        return -ENOMEM;
    }

    /* Set up ring pointers */
    rings->rx = (struct rattan_rx_ring *)((char *)rings->mapped + RATTAN_RX_RING_OFFSET);
    rings->tx = (struct rattan_tx_ring *)((char *)rings->mapped + RATTAN_TX_RING_OFFSET);
    rings->fill = (struct rattan_fill_ring *)((char *)rings->mapped + RATTAN_FILL_RING_OFFSET);
    rings->comp = (struct rattan_comp_ring *)((char *)rings->mapped + RATTAN_COMP_RING_OFFSET);
    rings->drop = (struct rattan_drop_ring *)((char *)rings->mapped + RATTAN_DROP_RING_OFFSET);

    /* Allocate kernel-private recycle ring */
    // rings->recycle = kzalloc(sizeof(struct rattan_comp_ring), GFP_KERNEL);
    // if (!rings->recycle) {
    // 	vunmap(rings->mapped);
    // 	rattan_unpin_pages(rings->pages, rings->nr_pages);
    // 	kvfree(rings->pages);
    // 	kfree(rings);
    // 	return -ENOMEM;
    // }
    // /* Initialize producer/consumer to zero */
    // rings->recycle->ring.producer = 0;
    // rings->recycle->ring.consumer = 0;

    rcu_assign_pointer(queue->rings, rings);
    pr_info("rattan%d:q%u rings registered: rx=%p tx=%p fill=%p comp=%p drop=%p\n", ctx->dev_id,
            qid, rings->rx, rings->tx, rings->fill, rings->comp, rings->drop);

    return 0;
}

static int rattan_reg_rings(struct rattan_vnic_ctx *ctx, struct rattan_rings_reg __user *argp) {
    struct rattan_rings_reg params;

    if (copy_from_user(&params, argp, sizeof(params)))
        return -EFAULT;

    return rattan_reg_rings_queue(ctx, 0, params.addr, params.len);
}

/* Sum a queue's per-cpu debug counters. Racy snapshot, fine for a dump. */
static void rattan_queue_dbg_sum(struct rattan_queue *queue, struct rattan_queue_dbg *sum) {
    const size_t n = sizeof(*sum) / sizeof(u64);
    u64 *dst = (u64 *)sum;
    int cpu;
    size_t i;

    BUILD_BUG_ON(sizeof(*sum) % sizeof(u64));
    memset(sum, 0, sizeof(*sum));
    for_each_possible_cpu(cpu) {
        const u64 *src = (const u64 *)per_cpu_ptr(queue->dbg, cpu);

        for (i = 0; i < n; i++)
            dst[i] += READ_ONCE(src[i]);
    }
}

static void rattan_debug_dump_queue_stats(struct rattan_queue *queue) {
    struct rattan_queue_dbg d;
    unsigned long interval;

    if (!debug_stats_interval_ms || !queue)
        return;

    interval = msecs_to_jiffies(debug_stats_interval_ms);
    if (!interval)
        interval = 1;

    if (!time_after_eq(jiffies, queue->dbg_last_dump_jiffies + interval))
        return;

    queue->dbg_last_dump_jiffies = jiffies;

    rattan_queue_dbg_sum(queue, &d);

    pr_info("rattan%d:q%u stats: xmit_ok=%llu rx_full=%llu fill_empty=%llu nomem=%llu "
            "invalid_addr=%llu notify=%llu kick_rx=%llu kick_rx_remote=%llu napi_polls=%llu "
            "napi_complete=%llu napi_budget_stop=%llu napi_tx_empty=%llu napi_comp_full=%llu "
            "napi_token_miss=%llu rx_ok=%llu rx_drop=%llu comp_ok=%llu comp_full=%llu "
            "recycled_token=%llu pending_tokens=%ld\n",
            queue->ctx->dev_id, queue->qid, d.xmit_ok, d.xmit_rx_full,
            d.xmit_fill_empty, d.xmit_nomem, d.xmit_invalid_addr, d.rx_notify, d.kick_rx,
            d.kick_rx_remote, d.napi_polls, d.napi_complete, d.napi_budget_stop, d.napi_tx_empty,
            d.napi_comp_full, d.napi_token_miss, d.napi_rx_ok, d.napi_rx_drop, d.comp_ok,
            d.comp_full, d.recycle_token, rattan_pending_count());

    pr_info("rattan%d:q%u ring: R%u T%u C%u F%u D%u", queue->ctx->dev_id, queue->qid,
            ring_count(&queue->rings->rx->ring), ring_count(&queue->rings->tx->ring),
            ring_count(&queue->rings->comp->ring), ring_count(&queue->rings->fill->ring),
            ring_count(&queue->rings->drop->ring));
}

/* ========== File Operations ========== */

static long rattan_vnic_ioctl(struct file *file, unsigned int cmd, unsigned long arg) {
    struct rattan_vnic_ctx *ctx = file->private_data;
    void __user *argp = (void __user *)arg;

    if (!ctx || !ctx->netdev)
        return -ENODEV;

    switch (cmd) {
    case RATTAN_VNIC_REG_UMEM:
        return rattan_reg_umem(ctx, argp);

    case RATTAN_VNIC_SHARE_UMEM:
        return rattan_share_umem(ctx, argp);

    case RATTAN_VNIC_REG_RINGS:
        return rattan_reg_rings(ctx, argp);

    case RATTAN_VNIC_REG_RINGS_Q: {
        struct rattan_rings_reg_q params;

        if (copy_from_user(&params, argp, sizeof(params)))
            return -EFAULT;

        return rattan_reg_rings_queue(ctx, params.queue_id, params.addr, params.len);
    }

    case RATTAN_VNIC_START: {
        struct rattan_start_config config;
        u16 qid;

        if (!rcu_access_pointer(ctx->umem))
            return -EINVAL;

        for (qid = 0; qid < ctx->num_queues; qid++) {
            if (!rcu_access_pointer(ctx->queues[qid].rings))
                return -EINVAL;
        }

        if (copy_from_user(&config, argp, sizeof(config)))
            return -EFAULT;

        /* Validate CPU number if specified */
        if (config.napi_cpu >= 0) {
            if (config.napi_cpu >= nr_cpu_ids)
                return -EINVAL;
            if (!cpu_online(config.napi_cpu))
                return -ENODEV;
        }

        /*
         * One NAPI CPU setting is applied to all queues for now.
         * Per-queue CPU placement can be added later if custom steering
         * or queue affinity control is introduced.
         */
        for (qid = 0; qid < ctx->num_queues; qid++) {
            ctx->queues[qid].napi_cpu = config.napi_cpu;
            napi_enable(&ctx->queues[qid].napi);
        }

        ctx->started = true;
        netif_carrier_on(ctx->netdev);
        pr_info("rattan%d: started (%u queues, napi_cpu=%d)\n", ctx->dev_id, ctx->num_queues,
                config.napi_cpu);
        return 0;
    }

    case RATTAN_VNIC_STOP: {
        u16 qid;

        ctx->started = false;
        netif_carrier_off(ctx->netdev);
        for (qid = 0; qid < ctx->num_queues; qid++)
            napi_disable(&ctx->queues[qid].napi);
        pr_info("rattan%d: stopped\n", ctx->dev_id);
        return 0;
    }

    case RATTAN_VNIC_READ_PAYLOAD:
        return rattan_read_payload(ctx, argp);

    case RATTAN_VNIC_RELEASE:
        return rattan_release_token(ctx, argp);

    case RATTAN_VNIC_KICK_RX:
        return rattan_kick_rx(rattan_get_queue(ctx, 0));

    case RATTAN_VNIC_KICK_RX_Q: {
        __u32 qid;

        if (copy_from_user(&qid, argp, sizeof(qid)))
            return -EFAULT;

        return rattan_kick_rx(rattan_get_queue(ctx, qid));
    }

    case RATTAN_VNIC_CLONE:
        return rattan_clone_token(ctx, argp);

    case RATTAN_VNIC_DEVICE_ID: {
        return rattan_device_id(ctx, argp);
    }

    default:
        pr_debug("rattan%d: unknown ioctl 0x%x\n", ctx->dev_id, cmd);
        return -ENOTTY;
    }
}

/*
 * File operations - open
 * Creates a new rattan network device
 */
static int rattan_vnic_fop_open(struct inode *inode, struct file *file) {
    struct rattan_vnic_ctx *ctx;
    struct net_device *netdev;
    u16 qid, queues;
    char name[IFNAMSIZ];
    int dev_id;
    int err;

    /* Allocate device ID using IDA (reuses freed IDs) */
    dev_id = ida_alloc_max(&rattan_ida, max_devices - 1, GFP_KERNEL);
    if (dev_id < 0)
        return dev_id;

    queues = num_queues > 0 ? num_queues : 1;

    /* Create multiqueue network device with embedded ctx */
    snprintf(name, IFNAMSIZ, "rattan%d", dev_id);
    netdev = alloc_netdev_mqs(sizeof(struct rattan_vnic_ctx), name, NET_NAME_UNKNOWN,
                              rattan_vnic_setup, queues, queues);
    if (!netdev) {
        err = -ENOMEM;
        goto err_alloc_netdev;
    }

    /* Initialize ctx (embedded in netdev priv area) */
    ctx = netdev_priv(netdev);
    ctx->netdev = netdev;
    ctx->dev_id = dev_id;
    ctx->started = false;
    ctx->num_queues = queues;
    ctx->queues = kcalloc(ctx->num_queues, sizeof(*ctx->queues), GFP_KERNEL);
    if (!ctx->queues) {
        err = -ENOMEM;
        goto err_alloc_queues;
    }

    RCU_INIT_POINTER(ctx->umem, NULL);
    init_waitqueue_head(&ctx->wait);

    for (qid = 0; qid < ctx->num_queues; qid++) {
        struct rattan_queue *queue = &ctx->queues[qid];

        queue->ctx = ctx;
        queue->qid = qid;
        RCU_INIT_POINTER(queue->rings, NULL);
        init_waitqueue_head(&queue->wait);

        rattan_hrtimer_setup(&queue->rx_timer, rattan_rx_timer_cb, CLOCK_MONOTONIC, HRTIMER_MODE_REL_SOFT);
        atomic_set(&queue->rx_pending, 0);

        queue->napi_cpu = -1;
        atomic_set(&queue->napi_scheduled, 0);
        queue->dbg = alloc_percpu(struct rattan_queue_dbg);
        if (!queue->dbg) {
            err = -ENOMEM;
            goto err_register;
        }
        queue->dbg_last_dump_jiffies = jiffies;
        rattan_netif_napi_add(netdev, &queue->napi, rattan_napi_poll);
    }

    err = netif_set_real_num_tx_queues(netdev, ctx->num_queues);
    if (err)
        goto err_register;

    err = netif_set_real_num_rx_queues(netdev, ctx->num_queues);
    if (err)
        goto err_register;

    /* Register network device */
    err = register_netdev(netdev);
    if (err) {
        pr_err("failed to register netdev %s: %d\n", name, err);
        goto err_register;
    }

    file->private_data = ctx;

    pr_info("created %s with %u queues\n", name, ctx->num_queues);
    return 0;

err_register:
    while (qid--) {
        netif_napi_del(&ctx->queues[qid].napi);
        free_percpu(ctx->queues[qid].dbg);
    }
    kfree(ctx->queues);
err_alloc_queues:
    free_netdev(netdev);
err_alloc_netdev:
    ida_free(&rattan_ida, dev_id);
    return err;
}

/*
 * File operations - release
 * Destroys the rattan network device
 */
static int rattan_vnic_fop_release(struct inode *inode, struct file *file) {
    struct rattan_vnic_ctx *ctx = file->private_data;
    struct rattan_umem *old_umem;
    struct rattan_rings **old_rings;
    u16 qid;

    if (!ctx)
        return 0;

    if (ctx->netdev) {
        int dev_id = ctx->dev_id;

        pr_info("destroying rattan%d\n", dev_id);

        if (ctx->started) {
            ctx->started = false;
            for (qid = 0; qid < ctx->num_queues; qid++)
                napi_disable(&ctx->queues[qid].napi);
        }

        for (qid = 0; qid < ctx->num_queues; qid++) {
            hrtimer_cancel(&ctx->queues[qid].rx_timer);
            netif_napi_del(&ctx->queues[qid].napi);
        }

        old_rings = kcalloc(ctx->num_queues, sizeof(*old_rings), GFP_KERNEL);
        if (!old_rings)
            return -ENOMEM;

        old_umem = rcu_access_pointer(ctx->umem);
        rcu_assign_pointer(ctx->umem, NULL);

        for (qid = 0; qid < ctx->num_queues; qid++) {
            old_rings[qid] = rcu_access_pointer(ctx->queues[qid].rings);
            rcu_assign_pointer(ctx->queues[qid].rings, NULL);
        }

        synchronize_rcu();

        /*
         * Note: We don't clean up pending SKBs here. They can still be
         * forwarded by other devices sharing the same UMEM, and are released
         * together with the UMEM once its last user is gone.
         */

        for (qid = 0; qid < ctx->num_queues; qid++)
            rattan_put_rings(old_rings[qid]);

        kfree(old_rings);
        rattan_put_umem(old_umem);

        unregister_netdev(ctx->netdev);
        for (qid = 0; qid < ctx->num_queues; qid++)
            free_percpu(ctx->queues[qid].dbg);
        kfree(ctx->queues);
        free_netdev(ctx->netdev);
        ida_free(&rattan_ida, dev_id);
    }

    return 0;
}

/*
 * Poll implementation - allows userspace to block until RX data is available
 *
 * Returns:
 *   EPOLLIN | EPOLLRDNORM - RX ring has data (producer != consumer)
 *   EPOLLERR - device not configured
 */
static __poll_t rattan_vnic_poll(struct file *file, poll_table *wait) {
    struct rattan_vnic_ctx *ctx = file->private_data;
    __poll_t mask = 0;
    bool configured = false;
    u16 qid;

    if (!ctx)
        return EPOLLERR;

    poll_wait(file, &ctx->wait, wait);

    rcu_read_lock();
    for (qid = 0; qid < ctx->num_queues; qid++) {
        struct rattan_rings *rings;
        u32 rx_prod, rx_cons;

        rings = rcu_dereference(ctx->queues[qid].rings);
        if (!rings || !rings->rx)
            continue;

        configured = true;

        /* Check RX ring: kernel produces, userspace consumes */
        rx_prod = smp_load_acquire(&rings->rx->ring.producer);
        rx_cons = READ_ONCE(rings->rx->ring.consumer);
        if (rx_prod != rx_cons) {
            mask |= EPOLLIN | EPOLLRDNORM;
            break;
        }
    }
    rcu_read_unlock();

    if (!configured)
        mask = EPOLLERR;

    return mask;
}

static const struct file_operations rattan_vnic_fops = {
    .owner = THIS_MODULE,
    .open = rattan_vnic_fop_open,
    .release = rattan_vnic_fop_release,
    .unlocked_ioctl = rattan_vnic_ioctl,
    .poll = rattan_vnic_poll,
};

static struct miscdevice rattan_vnic_misc = {
    .minor = MISC_DYNAMIC_MINOR,
    .name = "rattan-vnic",
    .fops = &rattan_vnic_fops,
    .mode = 0666,
};

static int __init rattan_vnic_init(void) {
    int err;

    rattan_cleanup_wq = alloc_workqueue("rattan_vnic_cleanup", 0, 0);
    if (!rattan_cleanup_wq)
        return -ENOMEM;

    err = misc_register(&rattan_vnic_misc);
    if (err) {
        pr_err("failed to register misc device: %d\n", err);
        destroy_workqueue(rattan_cleanup_wq);
        return err;
    }

    /* Initialize periodic GC workqueue */
    INIT_DELAYED_WORK(&gc_work, rattan_gc_work_fn);
    schedule_delayed_work(&gc_work, msecs_to_jiffies(RATTAN_GC_INTERVAL_MS));
    gc_work_initialized = true;

    pr_info("loaded (max_devices=%d, gc_interval=%ds)\n", max_devices,
            RATTAN_GC_INTERVAL_MS / 1000);
    return 0;
}

static void __exit rattan_vnic_exit(void) {
    /* Cancel periodic GC work before cleaning up */
    if (gc_work_initialized) {
        cancel_delayed_work_sync(&gc_work);
        gc_work_initialized = false;
    }

    misc_deregister(&rattan_vnic_misc);

    /*
     * Every fd is closed by now (the module is pinned while one is open), so
     * every UMEM has been freed and its pending SKBs with it.
     */
    WARN_ON(!list_empty(&rattan_umems));

    /* Drains any rings cleanup still queued by the last puts */
    destroy_workqueue(rattan_cleanup_wq);

    pr_info("unloaded\n");
}

module_init(rattan_vnic_init);
module_exit(rattan_vnic_exit);

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Rattan Project");
MODULE_DESCRIPTION("Rattan Virtual NIC Driver");
MODULE_VERSION(DRV_VERSION);
