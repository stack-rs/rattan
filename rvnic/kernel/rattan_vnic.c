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
#include <linux/hashtable.h>
#include <linux/hrtimer.h>
#include <linux/idr.h>
#include <linux/init.h>
#include <linux/jiffies.h>
#include <linux/kernel.h>
#include <linux/ktime.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/netdevice.h>
#include <linux/poll.h>
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
                 "Periodic per-queue debug stats dump interval in ms (0 = disabled)");

/* Device management - use IDA for unique device IDs */
static DEFINE_IDA(rattan_ida);

/* Next token value - simple monotonic counter for unique token generation */
static atomic64_t next_token = ATOMIC64_INIT(1);

/* Token lookup table with per-bucket locks */
static struct skb_bucket token_table[SKB_HASH_SIZE];
static atomic_t pending_token_count = ATOMIC_INIT(0);

/* Slab cache for skb_entry allocations */
static struct kmem_cache *skb_entry_cache;

/* Periodic GC workqueue */
static struct delayed_work gc_work;
static bool gc_work_initialized;

static const struct file_operations rattan_vnic_fops;

static int rattan_vnic_open(struct net_device *dev);
static int rattan_vnic_stop(struct net_device *dev);
static netdev_tx_t rattan_vnic_start_xmit(struct sk_buff *skb, struct net_device *dev);

static bool fill_ring_pop(struct rattan_fill_ring *fill, u64 *addr);
static bool rx_ring_has_space(struct rattan_rx_ring *rx);
static int rx_ring_push(struct rattan_rx_ring *rx, struct rattan_desc *desc);
static bool comp_ring_push(struct rattan_comp_ring *comp, u64 addr);
static bool comp_ring_has_space(struct rattan_comp_ring *comp);
static bool comp_ring_pop(struct rattan_comp_ring *comp, u64 *addr);
static bool release_token(u64 token);
static bool rattan_recycle_drop_token(struct rattan_drop_ring *drop);
static bool try_get_free_addr(struct rattan_queue *queue, struct rattan_rings *rings, u64 *addr);
static bool tx_ring_peek(struct rattan_tx_ring *tx, struct rattan_desc *desc);
static void tx_ring_advance(struct rattan_tx_ring *tx);
static void rattan_add_skb_entry(struct skb_entry *entry);
static int rattan_napi_poll(struct napi_struct *napi, int budget);
static enum hrtimer_restart rattan_rx_timer_cb(struct hrtimer *timer);
static struct rattan_queue *rattan_get_queue(struct rattan_vnic_ctx *ctx, u32 qid);
static struct rattan_queue *rattan_get_xmit_queue(struct rattan_vnic_ctx *ctx, struct sk_buff *skb);
static struct rattan_rings *rattan_get_rings(struct rattan_queue *queue);
static void rattan_put_rings(struct rattan_rings *rings);
static bool rattan_complete_chunk(struct rattan_queue *queue, struct rattan_rings *rings, u64 addr);
static void rattan_release_entry(struct skb_entry *entry, struct rattan_queue *queue);
static void rattan_debug_dump_queue_stats(struct rattan_queue *queue);

/* Helper to get bucket for a token */
static inline struct skb_bucket *token_to_bucket(u64 token) {
    return &token_table[hash_64(token, SKB_HASH_BITS)];
}

/* ========== Net Device Operations ========== */

static const struct net_device_ops rattan_netdev_ops = {
    .ndo_open = rattan_vnic_open,
    .ndo_stop = rattan_vnic_stop,
    .ndo_start_xmit = rattan_vnic_start_xmit,
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
            wake_up_interruptible_sync_poll(&queue->wait, EPOLLIN | EPOLLRDNORM);
        if (waitqueue_active(&ctx->wait))
            wake_up_interruptible_sync_poll(&ctx->wait, EPOLLIN | EPOLLRDNORM);
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
static void rattan_rx_notify(struct rattan_queue *queue) {
    struct rattan_vnic_ctx *ctx = queue->ctx;
    int pending;

    atomic64_inc(&queue->dbg_rx_notify);
    pending = atomic_inc_return(&queue->rx_pending);

    if (pending >= RATTAN_RX_BATCH_SIZE) {
        /* Batch full - wake immediately */
        atomic_set(&queue->rx_pending, 0);
        hrtimer_try_to_cancel(&queue->rx_timer);
        if (waitqueue_active(&queue->wait))
            wake_up_interruptible_sync_poll(&queue->wait, EPOLLIN | EPOLLRDNORM);
        if (waitqueue_active(&ctx->wait))
            wake_up_interruptible_sync_poll(&ctx->wait, EPOLLIN | EPOLLRDNORM);
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
                           struct rattan_rings *rings, struct rattan_umem *umem) {
    struct skb_entry *entry;
    struct rattan_desc desc;
    u64 chunk_addr;
    void *chunk_ptr;
    u32 copy_len;

    /* Check RX ring has space */
    if (!rx_ring_has_space(rings->rx)) {
        atomic64_inc(&queue->dbg_xmit_rx_full);
        pr_warn_ratelimited("rattan%d:q%u xmit blocked: RX ring full\n", queue->ctx->dev_id,
                            queue->qid);
        return -ENOSPC;
    }

    /* Pre-allocate SKB tracking entry */
    entry = kmem_cache_alloc(skb_entry_cache, GFP_ATOMIC);
    if (!entry) {
        atomic64_inc(&queue->dbg_xmit_nomem);
        pr_warn_ratelimited("rattan%d:q%u xmit blocked: skb_entry alloc failed\n",
                            queue->ctx->dev_id, queue->qid);
        return -ENOMEM;
    }

    /* Get chunk from COMP or FILL ring */
    if (!try_get_free_addr(queue, rings, &chunk_addr)) {
        atomic64_inc(&queue->dbg_xmit_fill_empty);
        pr_warn_ratelimited("rattan%d:q%u xmit blocked: no chunk available\n", queue->ctx->dev_id,
                            queue->qid);
        kmem_cache_free(skb_entry_cache, entry);
        return -ENOBUFS;
    }

    /* Validate chunk address */
    if (chunk_addr % umem->chunk_size != 0 || chunk_addr + umem->chunk_size > umem->len) {
        atomic64_inc(&queue->dbg_xmit_invalid_addr);
        pr_warn_ratelimited("rattan%d:q%u invalid chunk addr 0x%llx\n", queue->ctx->dev_id,
                            queue->qid, chunk_addr);
        kmem_cache_free(skb_entry_cache, entry);
        return -EINVAL;
    }

    /* Copy header to UMEM chunk */
    copy_len = min_t(u32, skb->len, RATTAN_HEADER_SIZE);
    chunk_ptr = (u8 *)umem->mapped + chunk_addr + umem->headroom;
    skb_copy_bits(skb, 0, chunk_ptr, copy_len);

    /* Generate unique token and setup entry */
    entry->token = atomic64_inc_return(&next_token);
    entry->skb = skb;
    entry->timestamp = ktime_get();
    entry->chunk_addr = chunk_addr;
    entry->has_chunk = true;
    entry->qid = queue->qid;
    entry->rings = rings;
    refcount_inc(&rings->refcount);

    /* Add to token table */
    rattan_add_skb_entry(entry);

    /* Build descriptor for userspace */
    desc.addr = chunk_addr;
    desc.len = copy_len;
    desc.options = skb->len & 0xffff;
    desc.token = entry->token;
    desc.timestamp = ktime_to_ns(entry->timestamp);

    rx_ring_push(rings->rx, &desc);
    atomic64_inc(&queue->dbg_xmit_ok);

    dev->stats.tx_packets++;
    dev->stats.tx_bytes += skb->len;

    /* Orphan the skb, or it will still occupy the TSQ credit
     * on the sender side.
     *
     * Copied from https://elixir.bootlin.com/linux/v6.12.74/source/drivers/net/tun.c#L1121-L1126
     */
    skb_orphan(skb);
    nf_reset_ct(skb);

    return 0;
}

static netdev_tx_t rattan_vnic_start_xmit(struct sk_buff *skb, struct net_device *dev) {
    struct rattan_vnic_ctx *ctx = netdev_priv(dev);
    struct rattan_queue *queue = NULL;
    struct rattan_rings *rings = NULL;
    struct rattan_umem *umem;
    struct sk_buff *segs, *next;
    int ret;
    bool wake = false;

    u64 start_timestamp = ktime_to_ns(ktime_get());
    u64 end_timestamp;

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

    rings = rattan_get_rings(queue);
    umem = rcu_dereference(ctx->umem);
    if (!rings || !umem)
        goto drop;

    /*
     * Handle GSO packets: segment them into individual MTU-sized SKBs.
     * This ensures each entry in the hash table is a standard packet,
     * which simplifies forwarding and allows per-packet processing.
     */
    if (skb_is_gso(skb)) {
        segs = skb_gso_segment(skb, 0);
        if (IS_ERR_OR_NULL(segs)) {
            dev_kfree_skb_any(skb);
            dev->stats.tx_dropped++;
            goto out;
        }

        /* Free the original GSO SKB */
        consume_skb(skb);

        /* Process each segment */
        while (segs) {
            next = segs->next;
            segs->next = NULL;

            ret = rattan_xmit_one(segs, dev, queue, rings, umem);
            if (ret < 0) {
                /* Drop this and remaining segments */
                dev_kfree_skb_any(segs);
                dev->stats.tx_dropped++;
                while (next) {
                    segs = next;
                    next = segs->next;
                    dev_kfree_skb_any(segs);
                    dev->stats.tx_dropped++;
                }
                goto out;
            }
            wake = true;
            segs = next;
        }
    } else {
        /* Non-GSO packet: process directly */
        ret = rattan_xmit_one(skb, dev, queue, rings, umem);
        if (ret < 0) {
            dev_kfree_skb_any(skb);
            dev->stats.tx_dropped++;
            goto out;
        }
        wake = true;
    }

out:
    if (rings)
        rattan_put_rings(rings);
    rcu_read_unlock();
    if (wake)
        rattan_rx_notify(queue);

    end_timestamp = ktime_to_ns(ktime_get());
    if (end_timestamp - start_timestamp > 100000) { // 0.1 ms
        pr_warn_ratelimited("Slow tx on normal path: %lld ns", end_timestamp - start_timestamp);
    }
    return NETDEV_TX_OK;

drop:
    if (rings)
        rattan_put_rings(rings);
    rcu_read_unlock();
    dev_kfree_skb_any(skb);
    dev->stats.tx_dropped++;

    end_timestamp = ktime_to_ns(ktime_get());
    if (end_timestamp - start_timestamp > 100000) { // 0.1 ms
        pr_warn_ratelimited("Slow tx on drop path: %lld ns", end_timestamp - start_timestamp);
    }
    return NETDEV_TX_OK;
}

/*
 * Net device setup - called by alloc_netdev
 */
static void rattan_vnic_setup(struct net_device *dev) {
    ether_setup(dev);

    dev->netdev_ops = &rattan_netdev_ops;

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

    if (rings->mapped)
        vunmap(rings->mapped);

    if (rings->pages) {
        rattan_unpin_pages(rings->pages, rings->nr_pages);
        kvfree(rings->pages);
    }

    // kfree(rings->recycle);
    kfree(rings);
}

static void rattan_put_rings(struct rattan_rings *rings) {
    if (!rings)
        return;

    if (refcount_dec_and_test(&rings->refcount))
        rattan_cleanup_rings(rings);
}

static bool rattan_complete_chunk(struct rattan_queue *queue, struct rattan_rings *rings,
                                  u64 addr) {
    bool ok;

    if (!rings)
        return false;
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
    ok = comp_ring_push(rings->comp, addr);
    spin_unlock_bh(&rings->comp_lock);

    if (queue) {
        if (ok)
            atomic64_inc(&queue->dbg_comp_ok);
        else
            atomic64_inc(&queue->dbg_comp_full);
    }

    if (!ok)
        pr_warn_ratelimited("rattan%d:q%u completion ring full, dropping chunk 0x%llx\n",
                            queue ? queue->ctx->dev_id : -1, queue ? queue->qid : 0, addr);

    return ok;
}

static void rattan_release_entry(struct skb_entry *entry, struct rattan_queue *queue) {
    if (!entry)
        return;

    if (entry->has_chunk && entry->rings)
        rattan_complete_chunk(queue, entry->rings, entry->chunk_addr);

    if (entry->rings)
        rattan_put_rings(entry->rings);

    if (entry->skb)
        dev_kfree_skb_any(entry->skb);

    kmem_cache_free(skb_entry_cache, entry);
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
static bool try_get_free_addr(struct rattan_queue *queue, struct rattan_rings *rings, u64 *addr) {
    bool ok;

    // As long as we only pop in `rattan_xmit_one`, we do not need to lock
    // spin_lock_bh(&rings->comp_lock);
    ok = comp_ring_pop(rings->comp, addr);
    // spin_unlock_bh(&rings->comp_lock);

    if (rattan_recycle_drop_token(rings->drop)) {
        atomic64_inc(&queue->dbg_recycle_token);
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
 * Check if COMP ring has space for at least one entry
 * Used for backpressure - check before consuming from TX ring
 */
static bool comp_ring_has_space(struct rattan_comp_ring *comp) {
    u32 cons, prod;

    prod = READ_ONCE(comp->ring.producer);
    cons = READ_ONCE(comp->ring.consumer);

    return (prod - cons) < RATTAN_RING_SIZE;
}

/*
 * Push chunk address to COMP ring (kernel is producer)
 * Returns true on success, false if ring full
 */
static bool comp_ring_push(struct rattan_comp_ring *comp, u64 addr) {
    u32 cons, prod;

    prod = READ_ONCE(comp->ring.producer);
    // Make sure that we see the updated value
    cons = smp_load_acquire(&comp->ring.consumer);

    if ((prod - cons) >= RATTAN_RING_SIZE)
        return false; /* Ring full */

    comp->addrs[prod & RATTAN_RING_MASK] = addr;

    smp_store_release(&comp->ring.producer, prod + 1);

    return true;
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
static bool rattan_recycle_drop_token(struct rattan_drop_ring *drop) {
    u32 cons, prod;
    u64 token;

    cons = READ_ONCE(drop->ring.consumer);
    prod = smp_load_acquire(&drop->ring.producer);

    if (cons == prod)
        return false;

    token = drop->tokens[cons & RATTAN_RING_MASK];

    smp_store_release(&drop->ring.consumer, cons + 1);

    return release_token(token);
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

/*
 * Find SKB entry by token in hash table
 * Caller must hold the bucket lock for this token
 */
static struct skb_entry *rattan_find_skb_entry_locked(struct skb_bucket *bucket, u64 token) {
    struct skb_entry *entry;

    hlist_for_each_entry(entry, &bucket->head, node) {
        if (entry->token == token)
            return entry;
    }
    return NULL;
}

/*
 * Add SKB entry to token table
 * Handles locking and pending count increment
 */
static void rattan_add_skb_entry(struct skb_entry *entry) {
    struct skb_bucket *bucket = token_to_bucket(entry->token);
    unsigned long flags;

    spin_lock_irqsave(&bucket->lock, flags);
    hlist_add_head(&entry->node, &bucket->head);
    spin_unlock_irqrestore(&bucket->lock, flags);
    atomic_inc(&pending_token_count);
}

/*
 * Read payload from tracked SKB
 * Note: ctx is unused now since tokens are global, but kept for API consistency
 */
static int rattan_read_payload(struct rattan_vnic_ctx *ctx,
                               struct rattan_payload_req __user *argp) {
    struct rattan_payload_req req;
    struct rattan_payload_resp resp;
    struct skb_bucket *bucket;
    struct skb_entry *entry;
    struct sk_buff *skb;
    unsigned long flags;
    void *kbuf;
    u32 copy_len;
    int err;

    (void)ctx; /* Unused - tokens are global */

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    bucket = token_to_bucket(req.token);
    spin_lock_irqsave(&bucket->lock, flags);
    entry = rattan_find_skb_entry_locked(bucket, req.token);
    if (!entry) {
        spin_unlock_irqrestore(&bucket->lock, flags);
        return -ENOENT;
    }

    skb = entry->skb;
    resp.total_len = skb->len;

    /* Calculate how much to copy */
    if (req.offset >= skb->len) {
        copy_len = 0;
        resp.len = 0;
        spin_unlock_irqrestore(&bucket->lock, flags);
        goto write_resp;
    }

    copy_len = min_t(u32, req.len, skb->len - req.offset);
    resp.len = copy_len;

    /* Take reference and release lock */
    skb_get(skb);
    spin_unlock_irqrestore(&bucket->lock, flags);

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
 * Release token - free the SKB associated with a token
 */
static inline bool release_token(u64 token) {
    struct skb_bucket *bucket;
    struct skb_entry *entry;
    unsigned long flags;

    bucket = token_to_bucket(token);
    spin_lock_irqsave(&bucket->lock, flags);
    entry = rattan_find_skb_entry_locked(bucket, token);
    if (!entry) {
        spin_unlock_irqrestore(&bucket->lock, flags);
        return false;
    }

    /* Remove from hash table */
    hlist_del(&entry->node);
    spin_unlock_irqrestore(&bucket->lock, flags);
    atomic_dec(&pending_token_count);

    /* The queue may have been released here, if the fd has been closed,
     * since the pointer to the queue is used for statistics and debugging only,
     * we pass NULL to avoid possible use-after-free.
     */
    rattan_release_entry(entry, NULL);

    return true;
}

/*
 * Release token, ioctl handler
 */
static int rattan_release_token(struct rattan_vnic_ctx *ctx,
                                struct rattan_release_req __user *argp) {
    struct rattan_release_req req;

    (void)ctx; /* Unused - tokens are global */

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    if (!release_token(req.token))
        return -ENOENT;

    return 0;
}

/*
 * Clone a token - create a new token with a cloned SKB
 */
static int rattan_clone_token(struct rattan_vnic_ctx *ctx, struct rattan_clone_req __user *argp) {
    struct rattan_clone_req req;
    struct rattan_clone_resp resp;
    struct skb_bucket *bucket;
    struct skb_entry *entry, *new_entry;
    struct sk_buff *new_skb;
    unsigned long flags;

    (void)ctx; /* Unused - tokens are global */

    if (copy_from_user(&req, argp, sizeof(req)))
        return -EFAULT;

    /* Allocate new entry first (outside lock) */
    new_entry = kmem_cache_alloc(skb_entry_cache, GFP_KERNEL);
    if (!new_entry)
        return -ENOMEM;

    bucket = token_to_bucket(req.token);
    spin_lock_irqsave(&bucket->lock, flags);
    entry = rattan_find_skb_entry_locked(bucket, req.token);
    if (!entry) {
        spin_unlock_irqrestore(&bucket->lock, flags);
        kmem_cache_free(skb_entry_cache, new_entry);
        return -ENOENT;
    }

    /* Copy the SKB (not clone - we need independent data for modifications) */
    new_skb = skb_copy(entry->skb, GFP_ATOMIC);
    if (!new_skb) {
        spin_unlock_irqrestore(&bucket->lock, flags);
        kmem_cache_free(skb_entry_cache, new_entry);
        return -ENOMEM;
    }

    /* Setup new entry - cloned tokens do not own the original RX chunk
       Notice that the source bucket lock is held during this operation */
    new_entry->token = atomic64_inc_return(&next_token);
    new_entry->skb = new_skb;
    new_entry->timestamp = entry->timestamp;
    new_entry->chunk_addr = 0;
    new_entry->has_chunk = false;
    new_entry->qid = entry->qid;
    new_entry->rings = NULL;

    spin_unlock_irqrestore(&bucket->lock, flags);

    /* Copy to userspace first - if this fails, no cleanup needed */
    resp.new_token = new_entry->token;
    if (copy_to_user(argp, &resp, sizeof(resp))) {
        kfree_skb(new_skb);
        kmem_cache_free(skb_entry_cache, new_entry);
        return -EFAULT;
    }

    /* Add to hash table */
    rattan_add_skb_entry(new_entry);

    return 0;
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
    struct skb_bucket *bucket;
    struct skb_entry *entry;
    unsigned long flags;
    int processed = 0;
    int pkt_len;

    atomic64_inc(&queue->dbg_napi_polls);
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

    while (processed < budget) {
        /* Backpressure: queue-local COMP ring must have space */
        if (!comp_ring_has_space(rings->comp)) {
            atomic64_inc(&queue->dbg_napi_comp_full);
            pr_warn_ratelimited("rattan%d:q%u NAPI blocked: COMP ring full\n", ctx->dev_id,
                                queue->qid);
            break;
        }

        /* Peek at next TX descriptor */
        if (!tx_ring_peek(rings->tx, &desc)) {
            atomic64_inc(&queue->dbg_napi_tx_empty);
            break; /* TX ring empty */
        }

        /* Look up token */
        bucket = token_to_bucket(desc.token);
        spin_lock_irqsave(&bucket->lock, flags);
        entry = rattan_find_skb_entry_locked(bucket, desc.token);
        if (!entry) {
            spin_unlock_irqrestore(&bucket->lock, flags);
            /* Token not found - consume descriptor, return TX chunk */
            tx_ring_advance(rings->tx);
            atomic64_inc(&queue->dbg_napi_token_miss);
            rattan_complete_chunk(queue, rings, desc.addr);
            pr_warn_ratelimited("rattan%d:q%u NAPI token miss: 0x%llx\n", ctx->dev_id, queue->qid,
                                desc.token);
            processed++;
            continue;
        }

        /* Consume descriptor and remove from hash */
        tx_ring_advance(rings->tx);
        hlist_del(&entry->node);
        spin_unlock_irqrestore(&bucket->lock, flags);
        atomic_dec(&pending_token_count);

        /* Copy modified header back to SKB */
        if (desc.len > 0) {
            if (desc.addr % umem->chunk_size == 0 && desc.addr + umem->chunk_size <= umem->len &&
                umem->headroom + desc.len <= umem->chunk_size) {
                void *header_ptr = (u8 *)umem->mapped + desc.addr + umem->headroom;
                u32 copy_len = min3(desc.len, entry->skb->len, (u32)RATTAN_HEADER_SIZE);
                skb_store_bits(entry->skb, 0, header_ptr, copy_len);
            }
        }

        /* Record length and prepare SKB for injection */
        pkt_len = entry->skb->len;
        entry->skb->dev = dev;
        skb_record_rx_queue(entry->skb, queue->qid);
        entry->skb->protocol = eth_type_trans(entry->skb, dev);

        /*
         * Use netif_receive_skb() instead of netif_rx() since we're
         * already in NAPI/softirq context on the target CPU.
         */
        if (netif_receive_skb(entry->skb) == NET_RX_SUCCESS) {
            atomic64_inc(&queue->dbg_napi_rx_ok);
            dev->stats.rx_packets++;
            dev->stats.rx_bytes += pkt_len;
        } else {
            atomic64_inc(&queue->dbg_napi_rx_drop);
            dev->stats.rx_dropped++;
        }

        entry->skb = NULL;
        if (!entry->has_chunk || desc.addr != entry->chunk_addr)
            rattan_complete_chunk(queue, rings, desc.addr);
        rattan_release_entry(entry, queue);
        processed++;
    }

    rcu_read_unlock();

    if (processed < budget) {
    complete:
        /* All work done - exit polling mode */
        atomic64_inc(&queue->dbg_napi_complete);
        napi_complete_done(napi, processed);
        atomic_set(&queue->napi_scheduled, 0);
    } else {
        atomic64_inc(&queue->dbg_napi_budget_stop);
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

    atomic64_inc(&queue->dbg_kick_rx);
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
            atomic64_inc(&queue->dbg_kick_rx_remote);
            smp_call_function_single(target_cpu, rattan_schedule_napi_ipi, queue, 0);
        }
    }

    return 0;
}

/*
 * Internal GC helper - collects old tokens from the hash table
 *
 * Iterates through all buckets, locking each one individually.
 * This allows concurrent operations on other buckets during GC.
 *
 * Returns number of tokens collected.
 */
static u32 rattan_do_gc(u32 timeout_ms, u32 max_collect) {
    unsigned long flags;
    ktime_t now, cutoff;
    u32 collected = 0;
    int bkt;

    now = ktime_get();
    cutoff = ktime_sub_ms(now, timeout_ms);

    for (bkt = 0; bkt < SKB_HASH_SIZE; bkt++) {
        struct skb_bucket *bucket = &token_table[bkt];

        for (;;) {
            struct skb_entry *entry = NULL;

            spin_lock_irqsave(&bucket->lock, flags);
            hlist_for_each_entry(entry, &bucket->head, node) {
                if (ktime_before(entry->timestamp, cutoff)) {
                    hlist_del(&entry->node);
                    atomic_dec(&pending_token_count);
                    break;
                }
            }
            spin_unlock_irqrestore(&bucket->lock, flags);

            if (!entry)
                break;

            /* The queue may have been released here, if the fd has been closed,
             * since the pointer to the queue is used for statistics and debugging only,
             * we pass NULL to avoid possible use-after-free.
             */
            rattan_release_entry(entry, NULL);
            collected++;

            /* Respect max_collect limit (0 = unlimited) */
            if (max_collect && collected >= max_collect)
                return collected;
        }
    }

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
    u32 remaining;

    collected = rattan_do_gc(RATTAN_TOKEN_TIMEOUT_MS, 0);
    remaining = atomic_read(&pending_token_count);

    if (collected > 0)
        pr_info("periodic GC: collected %u orphaned tokens, %u remaining\n", collected, remaining);

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
    umem->headroom = params.headroom;
    umem->nr_chunks = params.len / params.chunk_size;

    nr_pages = (params.len + PAGE_SIZE - 1) / PAGE_SIZE;
    umem->pages = kvmalloc_array(nr_pages, sizeof(struct page *), GFP_KERNEL);
    if (!umem->pages) {
        kfree(umem);
        return -ENOMEM;
    }

    ret = rattan_pin_pages(params.addr, nr_pages, FOLL_WRITE, umem->pages);
    if (ret < 0) {
        kvfree(umem->pages);
        kfree(umem);
        return ret;
    }

    if (ret != nr_pages) {
        rattan_unpin_pages(umem->pages, ret);
        kvfree(umem->pages);
        kfree(umem);
        return -ENOMEM;
    }

    umem->nr_pages = nr_pages;

    umem->mapped = vmap(umem->pages, nr_pages, VM_MAP, PAGE_KERNEL);
    if (!umem->mapped) {
        rattan_unpin_pages(umem->pages, nr_pages);
        kvfree(umem->pages);
        kfree(umem);
        return -ENOMEM;
    }

    /* Initialize refcount to 1 (this device owns it) */
    refcount_set(&umem->refcount, 1);

    rcu_assign_pointer(ctx->umem, umem);
    pr_info("rattan%d: UMEM registered: %u chunks of %u bytes (headroom=%u)\n", ctx->dev_id,
            umem->nr_chunks, umem->chunk_size, umem->headroom);

    return 0;
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

static void rattan_debug_dump_queue_stats(struct rattan_queue *queue) {
    unsigned long interval;

    if (!debug_stats_interval_ms || !queue)
        return;

    interval = msecs_to_jiffies(debug_stats_interval_ms);
    if (!interval)
        interval = 1;

    if (!time_after_eq(jiffies, queue->dbg_last_dump_jiffies + interval))
        return;

    queue->dbg_last_dump_jiffies = jiffies;

    pr_info("rattan%d:q%u stats: xmit_ok=%lld rx_full=%lld fill_empty=%lld nomem=%lld "
            "invalid_addr=%lld notify=%lld kick_rx=%lld kick_rx_remote=%lld napi_polls=%lld "
            "napi_complete=%lld napi_budget_stop=%lld napi_tx_empty=%lld napi_comp_full=%lld "
            "napi_token_miss=%lld rx_ok=%lld rx_drop=%lld comp_ok=%lld comp_full=%lld "
            "recycled_token=%lld pending_tokens=%d\n",
            queue->ctx->dev_id, queue->qid, atomic64_read(&queue->dbg_xmit_ok),
            atomic64_read(&queue->dbg_xmit_rx_full), atomic64_read(&queue->dbg_xmit_fill_empty),
            atomic64_read(&queue->dbg_xmit_nomem), atomic64_read(&queue->dbg_xmit_invalid_addr),
            atomic64_read(&queue->dbg_rx_notify), atomic64_read(&queue->dbg_kick_rx),
            atomic64_read(&queue->dbg_kick_rx_remote), atomic64_read(&queue->dbg_napi_polls),
            atomic64_read(&queue->dbg_napi_complete), atomic64_read(&queue->dbg_napi_budget_stop),
            atomic64_read(&queue->dbg_napi_tx_empty), atomic64_read(&queue->dbg_napi_comp_full),
            atomic64_read(&queue->dbg_napi_token_miss), atomic64_read(&queue->dbg_napi_rx_ok),
            atomic64_read(&queue->dbg_napi_rx_drop), atomic64_read(&queue->dbg_comp_ok),
            atomic64_read(&queue->dbg_comp_full), atomic64_read(&queue->dbg_recycle_token),
            atomic_read(&pending_token_count));

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
        atomic64_set(&queue->dbg_xmit_ok, 0);
        atomic64_set(&queue->dbg_xmit_rx_full, 0);
        atomic64_set(&queue->dbg_xmit_fill_empty, 0);
        atomic64_set(&queue->dbg_xmit_nomem, 0);
        atomic64_set(&queue->dbg_xmit_invalid_addr, 0);
        atomic64_set(&queue->dbg_rx_notify, 0);
        atomic64_set(&queue->dbg_kick_rx, 0);
        atomic64_set(&queue->dbg_kick_rx_remote, 0);
        atomic64_set(&queue->dbg_napi_polls, 0);
        atomic64_set(&queue->dbg_napi_complete, 0);
        atomic64_set(&queue->dbg_napi_budget_stop, 0);
        atomic64_set(&queue->dbg_napi_tx_empty, 0);
        atomic64_set(&queue->dbg_napi_comp_full, 0);
        atomic64_set(&queue->dbg_napi_token_miss, 0);
        atomic64_set(&queue->dbg_napi_rx_ok, 0);
        atomic64_set(&queue->dbg_napi_rx_drop, 0);
        atomic64_set(&queue->dbg_comp_ok, 0);
        atomic64_set(&queue->dbg_comp_full, 0);
        atomic64_set(&queue->dbg_recycle_token, 0);
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
    while (qid--)
        netif_napi_del(&ctx->queues[qid].napi);
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
         * forwarded by other devices sharing the same UMEM. Any orphaned
         * SKBs will be cleaned up when the module unloads.
         */

        for (qid = 0; qid < ctx->num_queues; qid++)
            rattan_put_rings(old_rings[qid]);

        kfree(old_rings);
        rattan_put_umem(old_umem);

        unregister_netdev(ctx->netdev);
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
    int i;

    /* Initialize per-bucket spinlocks for SKB hash table */
    for (i = 0; i < SKB_HASH_SIZE; i++) {
        INIT_HLIST_HEAD(&token_table[i].head);
        spin_lock_init(&token_table[i].lock);
    }

    /* Create slab cache for SKB tracking entries */
    skb_entry_cache = kmem_cache_create("rattan_skb_entry", sizeof(struct skb_entry), 0,
                                        SLAB_HWCACHE_ALIGN, NULL);
    if (!skb_entry_cache) {
        pr_err("failed to create skb entry cache\n");
        return -ENOMEM;
    }

    err = misc_register(&rattan_vnic_misc);
    if (err) {
        pr_err("failed to register misc device: %d\n", err);
        kmem_cache_destroy(skb_entry_cache);
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
    unsigned long flags;
    int bkt;
    int leaked = 0;

    /* Cancel periodic GC work before cleaning up */
    if (gc_work_initialized) {
        cancel_delayed_work_sync(&gc_work);
        gc_work_initialized = false;
    }

    misc_deregister(&rattan_vnic_misc);

    /*
     * Clean up any remaining SKB entries in the hash table.
     * This can happen if devices were closed while packets were pending.
     * Iterate through each bucket with its own lock.
     */
    for (bkt = 0; bkt < SKB_HASH_SIZE; bkt++) {
        struct skb_bucket *bucket = &token_table[bkt];

        for (;;) {
            struct skb_entry *entry = NULL;

            spin_lock_irqsave(&bucket->lock, flags);
            if (!hlist_empty(&bucket->head)) {
                entry = hlist_entry(bucket->head.first, struct skb_entry, node);
                hlist_del(&entry->node);
            }
            spin_unlock_irqrestore(&bucket->lock, flags);

            if (!entry)
                break;
            /* The queue should have been released here, as the fd has been closed,
             * since the pointer to the queue is used for statistics and debugging only,
             * we pass NULL to avoid possible use-after-free.
             */
            rattan_release_entry(entry, NULL);
            leaked++;
        }
    }

    if (leaked > 0)
        pr_info("cleaned up %d leaked SKB entries\n", leaked);

    kmem_cache_destroy(skb_entry_cache);
    pr_info("unloaded\n");
}

module_init(rattan_vnic_init);
module_exit(rattan_vnic_exit);

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Rattan Project");
MODULE_DESCRIPTION("Rattan Virtual NIC Driver");
MODULE_VERSION(DRV_VERSION);
