/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Rattan Virtual NIC - Kernel Internal Header
 *
 * Copyright (C) 2025 Rattan Project
 *
 * This header contains kernel-internal definitions.
 * For the userspace API, see rattan_vnic_uapi.h.
 */

#ifndef _RATTAN_VNIC_INTERNAL_H
#define _RATTAN_VNIC_INTERNAL_H

#include <linux/jiffies.h>
#include <linux/ktime.h>
#include <linux/list.h>
#include <linux/netdevice.h>
#include <linux/refcount.h>
#include <linux/skbuff.h>
#include <linux/spinlock.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include "rattan_vnic_uapi.h"

/*
 * Driver identification
 */
#define DRV_NAME "rattan_vnic"
#define DRV_VERSION "0.1.0"

/*
 * Internal configuration constants
 */
#define RATTAN_TOKEN_TIMEOUT_MS 30000 /* Token timeout for GC (30 seconds) */
#define RATTAN_GC_INTERVAL_MS 600000  /* GC workqueue interval (10 minutes) */
#define RATTAN_RX_BATCH_SIZE 32       /* Wake up every N packets */
#define RATTAN_RX_TIMEOUT_NS 64000    /* Or after N nanoseconds (64µs) */

/*
 * Pending-packet slots - one per UMEM chunk
 *
 * Every packet handed to userspace is bound to exactly one UMEM chunk, so the
 * chunk index doubles as the slot index and the token is simply
 *
 *     token = gen << 32 | chunk_index
 *
 * where gen is a per-slot generation, bumped on every insert, that makes stale
 * or replayed tokens miss instead of aliasing onto whatever packet occupies the
 * chunk now. gen is never 0, so neither is a valid token.
 *
 * The slot state word packs gen with a FREE/LIVE/BUSY tag and every ownership
 * change is a single cmpxchg on it. A correct userspace never makes two CPUs
 * contend for one slot (the chunk's owner is also the slot's owner), but the
 * device node is world-accessible, so a duplicated token or a live chunk put
 * back on FILL must fail the cmpxchg rather than double-free the skb.
 *
 *   FREE(g) --insert--> BUSY(g) --publish--> LIVE(g+1)
 *   LIVE(g) --claim---> BUSY(g) --release--> FREE(g)   (or back to LIVE(g))
 *
 * BUSY is only ever held for a few non-sleeping instructions, and a claimer that
 * finds it treats the token as a miss rather than waiting.
 *
 * Chunks are returned to the COMP ring of the queue that originally supplied
 * them (slot->rings), which preserves per-queue buffer ownership.
 */
#define RATTAN_SLOT_FREE 0
#define RATTAN_SLOT_LIVE 1
#define RATTAN_SLOT_BUSY 2
#define RATTAN_SLOT_TAG_MASK 3

#define RATTAN_SLOT_STATE(gen, tag) (((u64)(u32)(gen) << 2) | (tag))
#define RATTAN_SLOT_GEN(state) ((u32)((state) >> 2))
#define RATTAN_TOKEN(idx, gen) (((u64)(u32)(gen) << 32) | (u32)(idx))
#define RATTAN_TOKEN_IDX(token) ((u32)(token))
#define RATTAN_TOKEN_GEN(token) ((u32)((token) >> 32))

struct rattan_rings;

struct rattan_pending {
    u64 state;                  /* RATTAN_SLOT_STATE(gen, tag) */
    struct sk_buff *skb;        /* The actual packet */
    struct rattan_rings *rings; /* Rings that own completion for this chunk */
    u32 stamp;                  /* jiffies at insert, for GC */
    u16 qid;                    /* Queue that provided the chunk */
};

/*
 * UMEM context - tracks pinned userspace memory
 *
 * UMEM can be shared between multiple devices (AF_XDP style).
 * The owner (first device to register) pins the pages and creates the mapping.
 * Other devices can share via SHARE_UMEM ioctl, incrementing the refcount.
 */
struct rattan_umem {
    refcount_t refcount; /* Number of devices using this UMEM */
    struct list_head node; /* On rattan_umems, for GC */
    struct page **pages;
    unsigned long nr_pages;
    void *mapped;   /* Kernel virtual address */
    u64 len;        /* Total length */
    u32 chunk_size; /* Chunk size */
    u32 headroom;   /* Headroom in each chunk */
    u32 nr_chunks;  /* Number of chunks */
    u8 chunk_shift; /* ilog2(chunk_size) */

    struct rattan_pending *pending; /* nr_chunks slots, indexed by chunk */
};

/*
 * Rings context - tracks pinned ring memory
 */
struct rattan_rings {
    refcount_t refcount;  /* References from queue + outstanding tokens */
    spinlock_t comp_lock; /* Serialize completion ring producers */

    struct page **pages;
    unsigned long nr_pages;
    void *mapped;
    u64 len;

    struct rattan_rx_ring *rx;
    struct rattan_tx_ring *tx;
    struct rattan_fill_ring *fill;
    struct rattan_comp_ring *comp;
    struct rattan_drop_ring *drop;

    /*
     * The last put can come from softirq (a token completed by NAPI, or
     * released via the DROP ring, after this queue's device closed), but
     * vunmap() and unpinning must not run there. The cleanup is deferred.
     */
    struct work_struct cleanup_work;
};

/*
 * Debug/instrumentation counters, per CPU so that the xmit and NAPI CPUs of a
 * queue never share a line. Only counted while debug_stats_interval_ms != 0.
 */
struct rattan_queue_dbg {
    u64 xmit_ok;           /* Packets enqueued to RX ring for userspace */
    u64 xmit_rx_full;      /* start_xmit blocked by RX ring full */
    u64 xmit_fill_empty;   /* start_xmit blocked by empty FILL ring */
    u64 xmit_nomem;        /* start_xmit blocked by allocation failure */
    u64 xmit_invalid_addr; /* start_xmit saw invalid chunk address */
    u64 rx_notify;         /* Userspace wake notifications */
    u64 kick_rx;           /* RX kick requests from userspace */
    u64 kick_rx_remote;    /* RX kicks sent via remote IPI */
    u64 napi_polls;        /* NAPI poll invocations */
    u64 napi_complete;     /* NAPI completion events */
    u64 napi_budget_stop;  /* NAPI stopped due to budget exhaustion */
    u64 napi_tx_empty;     /* NAPI found TX ring empty */
    u64 napi_comp_full;    /* NAPI blocked by COMP ring full */
    u64 napi_token_miss;   /* NAPI saw TX token lookup miss */
    u64 napi_rx_ok;        /* Packets injected into kernel RX path */
    u64 napi_rx_drop;      /* Packets dropped by kernel RX path */
    u64 comp_ok;           /* Chunk completions pushed to COMP ring */
    u64 comp_full;         /* Chunk completions dropped due to full COMP */
    u64 recycle_token;     /* Tokens recycled successfully */
};

/*
 * Per-device context - embedded in netdev private area
 */
struct rattan_queue {
    struct rattan_vnic_ctx *ctx;
    u16 qid;
    struct rattan_rings __rcu *rings;

    /*
     * Poll wait queue - for blocking waits on RX availability.
     * The device still keeps a device-global wait queue so a single fd can
     * be polled for activity from any queue.
     */
    wait_queue_head_t wait;

    /*
     * RX wake-up coalescing - batch wake-ups to reduce overhead
     * Similar to NIC interrupt coalescing (ethtool -C rx-frames/rx-usecs)
     */
    struct hrtimer rx_timer; /* Timer for coalescing timeout */
    atomic_t rx_pending;     /* Packets since last wake-up */

    struct napi_struct napi; /* NAPI structure for RX injection */
    int napi_cpu;            /* Target CPU (-1 = use caller's CPU) */
    atomic_t napi_scheduled; /* Avoid redundant IPIs for remote NAPI */

    /* Debug counters (struct rattan_queue_dbg), see RATTAN_DBG_INC */
    struct rattan_queue_dbg __percpu *dbg;
    unsigned long dbg_last_dump_jiffies; /* Last periodic stats dump time */
};

struct rattan_vnic_ctx {
    struct net_device *netdev;
    int dev_id;
    bool started;

    struct rattan_umem __rcu *umem;
    u16 num_queues;
    struct rattan_queue *queues;

    /* Device-global poll wait queue for activity from any queue */
    wait_queue_head_t wait;
};

#endif /* _RATTAN_VNIC_INTERNAL_H */
