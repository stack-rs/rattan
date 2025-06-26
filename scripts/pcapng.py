import struct
import dpkt.pcapng
import sys
from collections import namedtuple
import time

# 定义结构体
LogEntryHeader = namedtuple('LogEntryHeader', ['length', 'type'])
GeneralPktHeader = namedtuple('GeneralPktHeader', ['length', 'pkt_action', 'type'])
ProtocolHeader = namedtuple('ProtocolHeader', ['length', 'type'])

def parse_packet_log(input_file, output_file):
    with open(input_file, 'rb') as f_in, open(output_file, 'wb') as f_out:
        writer = dpkt.pcapng.Writer(f_out)
        packet_count = 0
        
        while True:
            try:
                # 1. 读取并解析Log Entry Header (4字节)
                lh_data = f_in.read(4)
                if len(lh_data) < 4:
                    break
                
                # 解析LH (大端序)
                lh_word1 = struct.unpack('>H', lh_data[:2])[0]
                lh_word2 = struct.unpack('>H', lh_data[2:4])[0]
                
                lh = LogEntryHeader(
                    length=(lh_word1 >> 4) & 0xFFF,  # 12 bits
                    type=lh_word1 & 0x0F             # 4 bits
                )
                
                gph = GeneralPktHeader(
                    length=(lh_word2 >> 8) & 0xFF,   # 8 bits
                    pkt_action=(lh_word2 >> 4) & 0x0F,  # 4 bits
                    type=lh_word2 & 0x0F             # 4 bits
                )
                
                # 2. 读取General Packet Entry (8字节)
                gp_data = f_in.read(8)
                if len(gp_data) < 8:
                    break
                
                gp_timestamp = struct.unpack('>I', gp_data[:4])[0]  # 32 bits
                gp_length = struct.unpack('>H', gp_data[4:6])[0]    # 16 bits
                
                # 3. 读取Protocol Header (4字节)
                prh_data = f_in.read(4)
                if len(prh_data) < 4:
                    break
                
                prh_word = struct.unpack('>H', prh_data[:2])[0]
                prh = ProtocolHeader(
                    length=(prh_word >> 4) & 0xFFF,  # 12 bits
                    type=prh_word & 0x0F             # 4 bits
                )
                
                # 4. 读取TCP Protocol Entry (20字节)
                tcp_data = f_in.read(20)
                if len(tcp_data) < 20:
                    break
                
                # 5. 创建空数据包
                packet_data = b""
                
                # 6. 写入pcapng文件(兼容不同dpkt版本)
                try:
                    # 方法1: 使用timestamp参数(微秒)
                    timestamp = int(gp_timestamp)  # 假设gp_timestamp已经是微秒
                    writer.writepkt(packet_data, timestamp=timestamp)
                except TypeError:
                    try:
                        # 方法2: 使用ts参数(秒)
                        ts_sec = gp_timestamp // 1000000
                        writer.writepkt(packet_data, ts=ts_sec)
                    except TypeError:
                        # 方法3: 不带时间戳
                        writer.writepkt(packet_data)
                
                packet_count += 1
                
            except Exception as e:
                print(f"处理错误: {str(e)}")
                continue
        
        print(f"转换完成! 共处理{packet_count}个数据包元数据记录")

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("用法: python log_to_pcapng.py 输入.log 输出.pcapng")
        sys.exit(1)
    
    parse_packet_log(sys.argv[1], sys.argv[2])