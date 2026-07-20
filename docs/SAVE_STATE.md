# X68S save-state format

Version 9 is a self-contained snapshot of mutable emulator state, but it is
deliberately not a software archive. IPL, CGROM, SCSI ROM and original floppy/
hard-disk bytes are never serialized into the payload.

The byte layout is:

1. `X68S` magic (4 bytes)
2. little-endian format version (2 bytes)
3. machine model (1 byte)
4. ROM/media manifest entry count (1 byte)
5. for each entry: UTF-8 slot-name length (1 byte), slot name, SHA-256 (32 bytes)
6. CRC-32 of the compressed payload (4 bytes)
7. LZ4 block with prepended uncompressed size

The payload contains the CPU, RAM, GVRAM, text VRAM, sprite RAM, device state,
SRAM, scheduler and copy-on-write overlays. On load, every manifest entry and
hash must exactly match the ROM/media currently attached to the same slot.
Only after that check does the core reattach the immutable bytes to the restored
mutable state. A mismatch returns `MachineError::StateMediaMismatch`.

Version 9 replaces the approximate floating-point OPM state with the YM2151
integer phase/envelope and resampler state. Earlier snapshots are rejected
rather than restoring an incompatible audio payload nondeterministically.

This design prevents state export from silently redistributing user ROMs or
disk images while still making state replay deterministic.
