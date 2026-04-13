/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * Rattan Virtual NIC - User API Header
 *
 * Copyright (C) 2025 Rattan Project
 *
 * This header defines the interface between kernel and userspace.
 * It should be kept in sync with userspace libraries (e.g., librvnic).
 */

#ifndef _RATTAN_VNIC_UAPI_H
#define _RATTAN_VNIC_UAPI_H

#include <linux/ioctl.h>
#include <linux/types.h>

/*
 * Configuration constants
 */
#define RATTAN_RING_SIZE 262144 /* Must be power of 2 */
#define RATTAN_RING_MASK (RATTAN_RING_SIZE - 1)
#define RATTAN_DEFAULT_CHUNK_SIZE 256      /* Default chunk size */
#define RATTAN_MIN_CHUNK_SIZE 256           /* Minimum chunk size */
#define RATTAN_MAX_CHUNK_SIZE 65536         /* Maximum chunk size (64KB) */
#define RATTAN_MAX_UMEM_SIZE (128ULL << 30) /* Maximum UMEM size (128 GiB) */
#define RATTAN_HEADER_SIZE 0               /* Bytes eagerly copied to chunk */

/*
 * UMEM registration parameters
 *
 * UMEM - Userspace Memory Pool
 *
 * Userspace allocates a contiguous memory region and registers it with
 * the kernel. The region is divided into fixed-size chunks.
 *
 * Layout:
 *   +----------+----------+----------+----------+----------+
 *   | chunk 0  | chunk 1  | chunk 2  |   ...    | chunk N  |
 *   +----------+----------+----------+----------+----------+
 *
 * Each chunk layout (with headroom):
 *   +----------+------------------------------------------+
 *   | headroom |              packet data                 |
 *   +----------+------------------------------------------+
 */
struct rattan_umem_reg {
    __u64 addr;       /* Userspace address of UMEM */
    __u64 len;        /* Total length of UMEM */
    __u32 chunk_size; /* Size of each chunk (must be power of 2) */
    __u32 headroom;   /* Headroom before packet data in each chunk */
};

/*
 * Ring header - producer/consumer indices (cache-line aligned)
 */
struct rattan_ring {
    __u32 producer __attribute__((aligned(64)));
    __u32 consumer __attribute__((aligned(64)));
    __u32 flags;
    __u32 _pad[13]; /* Pad to cache line boundary */
};

/* Ring flags (reserved for future use) */
#define RATTAN_RING_NEED_WAKEUP (1 << 0)

/*
 * Packet descriptor - used in both RX and TX rings
 */
struct rattan_desc {
    __u64 addr;      /* Chunk address (offset in UMEM) */
    __u32 len;       /* Packet/header length */
    __u32 options;   /* Reserved for future use */
    __u64 token;     /* Token for payload retrieval / forwarding */
    __u64 timestamp; /* Timestamp (ns) - RX: arrival, TX: reserved */
};

/*
 * FILL Ring - Userspace provides free chunks for kernel to use
 *
 * Flow:
 *   1. Userspace writes chunk addresses to fill ring (producer)
 *   2. Kernel reads chunk addresses when RX packet arrives (consumer)
 *   3. Kernel writes packet header to chunk, puts descriptor in RX ring
 *
 * Producer: Userspace
 * Consumer: Kernel
 */
struct rattan_fill_ring {
    struct rattan_ring ring;
    __u64 addrs[RATTAN_RING_SIZE]; /* Chunk addresses (offset in UMEM) */
};

struct rattan_drop_ring {
    struct rattan_ring ring;
    __u64 tokens[RATTAN_RING_SIZE]; /* tokens */
};

/*
 * COMP Ring - Kernel returns chunks that are done
 *
 * Flow:
 *   1. When token is released (via RELEASE ioctl or TX with release flag)
 *   2. Kernel writes chunk address to completion ring (producer)
 *   3. Userspace reads to reclaim chunks (consumer)
 *
 * Producer: Kernel
 * Consumer: Userspace
 */
struct rattan_comp_ring {
    struct rattan_ring ring;
    __u64 addrs[RATTAN_RING_SIZE]; /* Completed chunk addresses */
};

/*
 * RX Ring - Kernel produces packet descriptors, userspace consumes
 *
 * Producer: Kernel
 * Consumer: Userspace
 */
struct rattan_rx_ring {
    struct rattan_ring ring;
    struct rattan_desc descs[RATTAN_RING_SIZE];
};

/*
 * TX Ring - Userspace produces descriptors to forward, kernel consumes
 *
 * Producer: Userspace
 * Consumer: Kernel
 */
struct rattan_tx_ring {
    struct rattan_ring ring;
    struct rattan_desc descs[RATTAN_RING_SIZE];
};

/*
 * Ring memory layout
 *
 * Userspace allocates a contiguous region containing all five rings:
 *   +--------------+--------------+---------------+---------------+--------------+
 *   |   RX Ring    |   TX Ring    |  FILL Ring    |  COMP Ring    |  DROP Ring   |
 *   +--------------+--------------+---------------+---------------+--------------+
 */
#define RATTAN_RINGS_SIZE                                                                          \
    (sizeof(struct rattan_rx_ring) + sizeof(struct rattan_tx_ring) +                               \
     sizeof(struct rattan_fill_ring) + sizeof(struct rattan_comp_ring) +                           \
     sizeof(struct rattan_drop_ring))

#define RATTAN_RX_RING_OFFSET 0
#define RATTAN_TX_RING_OFFSET sizeof(struct rattan_rx_ring)
#define RATTAN_FILL_RING_OFFSET (RATTAN_TX_RING_OFFSET + sizeof(struct rattan_tx_ring))
#define RATTAN_COMP_RING_OFFSET (RATTAN_FILL_RING_OFFSET + sizeof(struct rattan_fill_ring))
#define RATTAN_DROP_RING_OFFSET (RATTAN_COMP_RING_OFFSET + sizeof(struct rattan_comp_ring))

/*
 * Ring registration parameters
 *
 * The original REG_RINGS ioctl continues to target queue 0 for backward
 * compatibility. Use struct rattan_rings_reg_q with REG_RINGS_Q to register
 * rings for a specific queue.
 */
struct rattan_rings_reg {
    __u64 addr; /* Userspace address of rings */
    __u64 len;  /* Total length (must be >= RATTAN_RINGS_SIZE) */
};

struct rattan_rings_reg_q {
    __u64 addr;     /* Userspace address of rings */
    __u64 len;      /* Total length (must be >= RATTAN_RINGS_SIZE) */
    __u32 queue_id; /* Queue to register rings for */
    __u32 _pad;
};

/*
 * Share UMEM from another device
 *
 * Instead of registering new UMEM, share an existing UMEM from another
 * rattan device. Pass the file descriptor of the device that owns the UMEM.
 *
 * Each device still needs its own rings (FILL/RX/TX/COMP), but chunks
 * can freely flow between devices sharing the same UMEM.
 */
struct rattan_share_umem_req {
    __s32 source_fd; /* fd of device that owns the UMEM */
    __u32 _pad;
};

/*
 * Payload read request/response
 */
struct rattan_payload_req {
    __u64 token;  /* Token from RX descriptor */
    __u64 buf;    /* Userspace buffer address */
    __u32 len;    /* Buffer length */
    __u32 offset; /* Offset within payload */
};

struct rattan_payload_resp {
    __u32 len;       /* Actual length copied */
    __u32 total_len; /* Total packet length */
};

/*
 * Token release request
 */
struct rattan_release_req {
    __u64 token; /* Token to release */
};

/*
 * Token clone request/response
 *
 * Clone an SKB to get a new token. This allows forwarding a packet
 * to multiple destinations.
 *
 * Each token has strict 1:1 mapping with an SKB.
 * Userspace manages UMEM chunks independently from tokens.
 */
struct rattan_clone_req {
    __u64 token; /* Token to clone */
};

struct rattan_clone_resp {
    __u64 new_token; /* New token for cloned SKB */
};

struct rattan_start_config {
    __s32 napi_cpu; /* Target CPU for NAPI (-1 = use caller's CPU) */
    __u32 _pad;
};

/*
 * ioctl commands
 */
#define RATTAN_VNIC_MAGIC 'R'

/* Setup commands */
#define RATTAN_VNIC_REG_UMEM _IOW(RATTAN_VNIC_MAGIC, 1, struct rattan_umem_reg)

#define RATTAN_VNIC_REG_RINGS _IOW(RATTAN_VNIC_MAGIC, 2, struct rattan_rings_reg)

#define RATTAN_VNIC_SHARE_UMEM _IOW(RATTAN_VNIC_MAGIC, 3, struct rattan_share_umem_req)

/* Queue-aware setup commands */
#define RATTAN_VNIC_REG_RINGS_Q _IOW(RATTAN_VNIC_MAGIC, 6, struct rattan_rings_reg_q)

/* Control commands */
#define RATTAN_VNIC_START _IOW(RATTAN_VNIC_MAGIC, 4, struct rattan_start_config)

#define RATTAN_VNIC_STOP _IO(RATTAN_VNIC_MAGIC, 5)

/* Data path commands */
#define RATTAN_VNIC_READ_PAYLOAD _IOWR(RATTAN_VNIC_MAGIC, 10, struct rattan_payload_req)

#define RATTAN_VNIC_RELEASE _IOW(RATTAN_VNIC_MAGIC, 11, struct rattan_release_req)

/*
 * Backward-compatible queue-0 RX kick.
 * Use KICK_RX_Q to target a specific queue.
 */
#define RATTAN_VNIC_KICK_RX _IO(RATTAN_VNIC_MAGIC, 12)

#define RATTAN_VNIC_CLONE _IOWR(RATTAN_VNIC_MAGIC, 13, struct rattan_clone_req)

#define RATTAN_VNIC_KICK_RX_Q _IOW(RATTAN_VNIC_MAGIC, 14, __u32)

// Get the device ID of the VNIC
#define RATTAN_VNIC_DEVICE_ID _IOR(RATTAN_VNIC_MAGIC, 15, __u32)

#endif /* _RATTAN_VNIC_UAPI_H */
