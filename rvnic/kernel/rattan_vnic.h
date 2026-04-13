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
#include <linux/netdevice.h>
#include <linux/refcount.h>
#include <linux/skbuff.h>
#include <linux/spinlock.h>
#include <linux/wait.h>

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
 * SKB tracking hash table configuration
 */
#define SKB_HASH_BITS 16 /* 2^16 = 65536 buckets */
#define SKB_HASH_SIZE (1 << SKB_HASH_BITS)

/*
 * SKB tracking entry - holds pending packets for payload retrieval
 *
 * This is stored in a GLOBAL hash table so tokens can be used across
 * different vNIC instances (e.g., receive on rattan0, forward via rattan1).
 *
 * Note: UMEM chunk management is handled entirely by userspace. The kernel
 * only tracks the SKB. When cloning for multicast, userspace allocates a
 * new chunk, copies the header, and passes the chunk address in the TX
 * descriptor.
 *
 * Chunks are returned to the COMP ring of the same queue/device that
 * originally supplied the chunk. This preserves per-queue buffer ownership
 * and means userspace should recycle completions back into the matching
 * queue's FILL ring.
 */
struct rattan_queue;

struct skb_entry {
    struct hlist_node node;     /* Hash table linkage */
    u64 token;                  /* Unique token for this packet */
    struct sk_buff *skb;        /* The actual packet */
    ktime_t timestamp;          /* When packet was received (for GC) */
    u64 chunk_addr;             /* Original RX chunk backing this token */
    bool has_chunk;             /* True if this entry owns chunk_addr */
    u16 qid;                    /* Queue that provided the original chunk */
    struct rattan_rings *rings; /* Rings that own completion for chunk_addr */
};

/*
 * Per-bucket locked hash table for SKB tracking
 *
 * Using per-bucket locks instead of a single global lock improves
 * scalability when multiple CPUs/devices are processing different tokens.
 */
struct skb_bucket {
    struct hlist_head head;
    spinlock_t lock;
} ____cacheline_aligned;

/*
 * UMEM context - tracks pinned userspace memory
 *
 * UMEM can be shared between multiple devices (AF_XDP style).
 * The owner (first device to register) pins the pages and creates the mapping.
 * Other devices can share via SHARE_UMEM ioctl, incrementing the refcount.
 */
struct rattan_umem {
    refcount_t refcount; /* Number of devices using this UMEM */
    struct page **pages;
    unsigned long nr_pages;
    void *mapped;   /* Kernel virtual address */
    u64 len;        /* Total length */
    u32 chunk_size; /* Chunk size */
    u32 headroom;   /* Headroom in each chunk */
    u32 nr_chunks;  /* Number of chunks */
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

    /* Debug/instrumentation counters */
    atomic64_t dbg_xmit_ok;              /* Packets enqueued to RX ring for userspace */
    atomic64_t dbg_xmit_rx_full;         /* start_xmit blocked by RX ring full */
    atomic64_t dbg_xmit_fill_empty;      /* start_xmit blocked by empty FILL ring */
    atomic64_t dbg_xmit_nomem;           /* start_xmit blocked by allocation failure */
    atomic64_t dbg_xmit_invalid_addr;    /* start_xmit saw invalid chunk address */
    atomic64_t dbg_rx_notify;            /* Userspace wake notifications */
    atomic64_t dbg_kick_rx;              /* RX kick requests from userspace */
    atomic64_t dbg_kick_rx_remote;       /* RX kicks sent via remote IPI */
    atomic64_t dbg_napi_polls;           /* NAPI poll invocations */
    atomic64_t dbg_napi_complete;        /* NAPI completion events */
    atomic64_t dbg_napi_budget_stop;     /* NAPI stopped due to budget exhaustion */
    atomic64_t dbg_napi_tx_empty;        /* NAPI found TX ring empty */
    atomic64_t dbg_napi_comp_full;       /* NAPI blocked by COMP ring full */
    atomic64_t dbg_napi_token_miss;      /* NAPI saw TX token lookup miss */
    atomic64_t dbg_napi_rx_ok;           /* Packets injected into kernel RX path */
    atomic64_t dbg_napi_rx_drop;         /* Packets dropped by kernel RX path */
    atomic64_t dbg_comp_ok;              /* Chunk completions pushed to COMP ring */
    atomic64_t dbg_comp_full;            /* Chunk completions dropped due to full COMP */
    atomic64_t dbg_recycle_token;        /* Tokens recycled successfully */
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
