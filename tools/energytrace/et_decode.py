#!/usr/bin/env python3
"""
XDS110 EnergyTrace decoder - reads endpoint 0x81 USB bulk traffic and decodes measurement data.

Based on captured USB traffic showing repeating ~40-byte packets with '0d01' prefix.
These are EnergyTrace samples streaming from the XDS110 via USB BULK IN endpoint 0x81.

Packet structure (inferred from pcap capture):
  Byte 0-1:    Packet type (0d01 = ET sample packet)
  Byte 2-4:    Timestamp/sequence counter
  Byte 5+:     Measurement data (current/voltage/energy in big-endian)
"""
import struct
import sys
from datetime import datetime


def parse_et_packet(data):
    """Parse a single 40-byte EnergyTrace sample packet from endpoint 0x81 IN."""
    if not data or len(data) < 16:
        return None

    # Scan for packet type indicator (0d01) - it may not be at offset 0
    # In captured traffic, 0d01 appears at byte offset 2
    found_offset = -1
    for i in range(min(10, len(data) - 1)):  # Search first 10 bytes for 0d01
        if data[i:i+2] == b'\x0d\x01':
            found_offset = i
            break

    if found_offset == -1:
        return None

    pkt_type = '0d01'

    # Data starts after the 0d01 marker
    data_start = found_offset + 2
    remaining_data = data[data_start:]

    # Next bytes (offset from 0d01): likely timestamp/sequence
    timestamp_raw = remaining_data[:4] if len(remaining_data) >= 4 else b'\x00\x00\x00\x00'
    timestamp = struct.unpack('>I', timestamp_raw)[0]

    # Following bytes: measurement values
    readings = {}
    offset = 4  # Start reading measurements after timestamp

    # Try both little-endian and big-endian to find plausible values
    # TI MSPM0 runs at ~3.3V, MSP430 typically 1.8-3.6V

    # Current field - 4 bytes
    if len(remaining_data) >= offset + 4:
        current_le = struct.unpack('<I', remaining_data[offset:offset+4])[0]
        current_be = struct.unpack('>I', remaining_data[offset:offset+4])[0]
        readings['current_le_raw'] = current_le
        readings['current_be_raw'] = current_be
        # Test which one gives plausible current (0.1mA - 100mA for MSPM0)
        if 10000 < current_le < 100000000:  # 10µA - 100mA in nA
            readings['current_amps'] = current_le / 1e9
            readings['current_endian'] = 'LE'
        elif 10000 < current_be < 100000000:
            readings['current_amps'] = current_be / 1e9
            readings['current_endian'] = 'BE'
        else:
            readings['current_amps'] = current_le / 1e9  # Fallback
            readings['current_endian'] = 'LE?'
        offset += 4

    # Voltage field - 2 bytes (try 10mV units like TI typically uses)
    if len(remaining_data) >= offset + 2:
        voltage_le = struct.unpack('<H', remaining_data[offset:offset+2])[0]
        voltage_be = struct.unpack('>H', remaining_data[offset:offset+2])[0]
        readings['voltage_le_raw'] = voltage_le
        readings['voltage_be_raw'] = voltage_be
        # Test for ~3300mV range in 10mV units
        if 200 < voltage_le < 500:  # 2000-5000mV
            readings['voltage_volts'] = voltage_le * 0.01
            readings['voltage_endian'] = 'LE'
        elif 200 < voltage_be < 500:
            readings['voltage_volts'] = voltage_be * 0.01
            readings['voltage_endian'] = 'BE'
        else:
            # Try 50mV units (common for Fixed Supply PDOs)
            if 50 < voltage_le < 100:
                readings['voltage_volts'] = voltage_le * 0.05
                readings['voltage_endian'] = 'LE(50mV)'
            elif 50 < voltage_be < 100:
                readings['voltage_volts'] = voltage_be * 0.05
                readings['voltage_endian'] = 'BE(50mV)'
            else:
                readings['voltage_volts'] = 3.3  # Fallback
                readings['voltage_endian'] = '???'
        offset += 2

    # Energy/Power field - 4 bytes
    if len(remaining_data) >= offset + 4:
        energy_le = struct.unpack('<I', remaining_data[offset:offset+4])[0]
        readings['energy_raw'] = energy_le
        readings['energy_likely_joules'] = energy_le / 1e7
        offset += 4

    # Remaining bytes might be power, temperature, or other metadata
    if len(remaining_data) > offset:
        readings['remaining_hex'] = remaining_data[offset:].hex()

    # Additional fields for remaining bytes (might be power, temperature, etc.)
    remaining = data[16:]
    if remaining:
        readings['extra_bytes'] = len(remaining)

    return {
        'timestamp': timestamp,
        'packet_type': pkt_type,
        'data': readings,
        'raw_len': len(data),
        'raw_hex': data.hex()
    }


def decode_pcap_stream(packets):
    """Decode a list of USB endpoint 0x81 packets."""
    samples = []
    for i, pkt_hex in enumerate(packets):
        if not pkt_hex:
            continue

        # Convert hex string to bytes
        data = bytes.fromhex(pkt_hex)

        # Parse the packet
        sample = parse_et_packet(data)
        if sample:
            sample['frame_idx'] = i
            samples.append(sample)
            print(f"Frame {i}: timestamp={sample['timestamp']:06x}")
            print(f"  Current: {sample['data'].get('current_amps', 'N/A'):e}" if 'current_amps' in sample['data'] else '  Current: N/A')
            print(f"  Voltage: {sample['data'].get('voltage_volts', 'N/A'):f}V" if 'voltage_volts' in sample['data'] else '  Voltage: N/A')
            print(f"  Energy:  {sample['data'].get('energy_joules', 'N/A'):e}J" if 'energy_joules' in sample['data'] else '  Energy:  N/A')
            print(f"  Raw:     {sample['raw_hex'][0:40]}...")
            print()

    return samples


if __name__ == '__main__':
    if len(sys.argv) < 2:
        print("Usage: et_decode.py PACKET_HEX_FILE")
        print("  PACKET_HEX_FILE: Text file with one hex packet per line")
        sys.exit(1)

    # Read hex packets from file (one per line, from the USB endpoint 0x81 traffic)
    with open(sys.argv[1], 'r') as f:
        packets = [line.strip() for line in f if line.strip()]

    print(f"Decoding {len(packets)} USB endpoint 0x81 packets...")
    print("=" * 80)

    samples = decode_pcap_stream(packets)

    print(f"\nDecoded {len(samples)} EnergyTrace samples")

    # Export to CSV
    if samples:
        outfile = '/tmp/etrace_samples.csv'
        with open(outfile, 'w') as f:
            f.write("timestamp,current_amps,voltage_volts,energy_joules\n")
            for s in samples:
                current = s['data'].get('current_amps', 0)
                voltage = s['data'].get('voltage_volts', 3.3)
                energy = s['data'].get('energy_joules', 0)
                f.write(f"{s['timestamp']},{current:.6e},{voltage:.4f},{energy:.6e}\n")
        print(f"Exported to: {outfile}")
