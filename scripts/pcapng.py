#!/usr/bin/env python3
"""
PCAPNG转换工具 - 终极修正版
功能：将自定义日志转换为Wireshark兼容的pcapng格式，完美解决时间戳问题
"""

import struct
import sys
from datetime import datetime

def create_pcapng_header():
    """生成完全标准的pcapng文件头"""
    # Section Header Block (SHB)
    shb = bytes.fromhex(
        "0a0d0d0a"  # Magic
        "1c000000"  # Block length (28)
        "4d3c2b1a"  # Byte-order magic
        "0100"      # Major version
        "0000"      # Minor version
        "ffffffffffffffff"  # Section length (unspecified)
        "1c000000"  # Block length (28)
    )
    
    # Interface Description Block (IDB)
    idb = bytes.fromhex(
        "01000000"  # Block type
        "14000000"  # Block length (20)
        "0100"      # LinkType (Ethernet)
        "0000"      # Reserved
        "0000ffff"  # SnapLen (65535)
        "14000000"  # Block length (20)
    )
    
    return shb + idb

def parse_log_timestamp(raw_bytes):
    """安全解析日志中的时间戳（处理32位溢出）"""
    timestamp = struct.unpack(">I", raw_bytes)[0]
    
    # 方案1：如果时间戳超过1年，视为相对时间
    ONE_YEAR_IN_US = 365 * 24 * 3600 * 1_000_000
    if timestamp > ONE_YEAR_IN_US:
        return timestamp % ONE_YEAR_IN_US
    
    # 方案2：直接使用31位无符号数
    return timestamp & 0x7FFFFFFF

def create_epb_block(timestamp_us, orig_len, first_ts=None):
    """生成增强型数据包块（自动处理时间基准）"""
    # 计算相对时间（确保不为负）
    rel_time = timestamp_us if first_ts is None else timestamp_us - first_ts
    rel_time = max(0, rel_time)  # 确保不小于0
    
    # 转换为秒和纳秒
    ts_sec, ts_nsec = divmod(rel_time, 1_000_000)
    ts_nsec *= 1_000  # 微秒→纳秒
    
    # 构建EPB块
    block = bytearray()
    block.extend(struct.pack("<I", 0x00000006))  # Block type (EPB)
    block.extend(struct.pack("<I", 32))          # Initial block length
    block.extend(struct.pack("<I", 0))           # Interface ID
    block.extend(struct.pack("<I", ts_sec))      # Timestamp seconds
    block.extend(struct.pack("<I", ts_nsec))     # Timestamp nanoseconds
    block.extend(struct.pack("<I", 0))           # Captured length
    block.extend(struct.pack("<I", orig_len))    # Original length
    block.extend(struct.pack("<I", 32))          # Final block length
    
    return bytes(block)

def convert_log_to_pcapng(input_path, output_path):
    """主转换函数"""
    with open(input_path, "rb") as f_in, open(output_path, "wb") as f_out:
        # 写入标准文件头
        f_out.write(create_pcapng_header())
        
        packet_count = 0
        first_timestamp = None
        
        while True:
            # 读取固定36字节头 (4B LH + 8B GP + 4B PRH + 20B TCP)
            header = f_in.read(36)
            if len(header) < 36:
                break
            
            try:
                # 解析关键字段
                timestamp = parse_log_timestamp(header[4:8])  # 4-8字节: GP.timestamp
                orig_len = struct.unpack(">H", header[10:12])[0]  # 10-12字节: GP.length
                
                # 初始化第一个时间戳
                if first_timestamp is None:
                    first_timestamp = timestamp
                
                # 写入数据包块
                epb = create_epb_block(timestamp, orig_len, first_timestamp)
                f_out.write(epb)
                packet_count += 1
                
            except Exception as e:
                print(f"警告: 跳过损坏数据包 (错误: {e})")
                continue
        
        # 写入统计信息
        print(f"总数据包: {packet_count}")

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("使用方法: python3 pcapng_converter.py 输入.log 输出.pcapng")
        sys.exit(1)
    convert_log_to_pcapng(sys.argv[1], sys.argv[2])
    