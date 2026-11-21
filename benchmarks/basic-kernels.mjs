export const RAM = 0x4000n;
const MASK64 = (1n << 64n) - 1n;
const pack = (values, width) =>
  values.reduce((v, x, i) => v | (BigInt(x) << BigInt(width * i)), 0n);
const little = (bytes, offset, value, width) => {
  for (let i = 0; i < width; i++) bytes[offset + i] = Number((value >> BigInt(i * 8)) & 255n);
};
const read = (bytes, offset, width) =>
  bytes.slice(offset, offset + width).reduce((v, b, i) => v | (BigInt(b) << BigInt(i * 8)), 0n);
const low32 = (old, value) => (old & ~0xffffffffn) | BigInt(value);
const low64 = (old, value) => (old & ~MASK64) | value;
const parity = (n) => {
  let bits = Number(n & 255n),
    count = 0;
  while (bits) {
    count += bits & 1;
    bits >>>= 1;
  }
  return count % 2 === 0 ? 4n : 0n;
};
const addFlags = (n) => 0x402n | parity(n) | (n % 16n === 0n ? 16n : 0n);
const incFlags = (n) => {
  const v = n & 255n;
  return (
    0x403n |
    parity(v) |
    (v % 16n === 0n ? 16n : 0n) |
    (v === 0n ? 64n : 0n) |
    (v & 128n) |
    (v === 128n ? 2048n : 0n)
  );
};
export const basicRows = [
  ...[4, 8].map((width) => ({
    name: `checked-vector-load-low${width * 8}`,
    body: width === 4 ? 'f30f1003' : 'f20f1003',
    instructions: 1,
    expected(s) {
      s.xmm[0] = read(s.ram, 16, width);
    },
  })),
  {
    name: 'scalar-add',
    body: '4883c001',
    instructions: 1,
    factor: 5,
    setup: (s) => {
      s.gpr[0] = 0n;
    },
    expected: (s, n) => {
      s.gpr[0] = n;
      s.flags = addFlags(n);
    },
  },
  {
    name: 'checked-scalar-load',
    body: '488b03',
    instructions: 1,
    factor: 5,
    setup: (s) => little(s.ram, 16, 7n, 8),
    expected: (s) => {
      s.gpr[0] = 7n;
    },
  },
  {
    name: 'checked-byte-rmw',
    body: 'fe03',
    instructions: 1,
    factor: 5,
    setup: (s) => {
      s.ram[16] = 0;
    },
    expected: (s, n) => {
      s.ram[16] = Number(n & 255n);
      s.flags = incFlags(n);
    },
  },
  {
    name: 'stack-push-pop',
    body: '5058',
    instructions: 2,
    factor: 5,
    setup: (s) => {
      s.gpr[0] = 0x123456789abcdef0n;
      s.gpr[4] = RAM + 48n;
    },
    expected: (s) => little(s.ram, 40, s.gpr[0], 8),
  },
  {
    name: 'signed-multiply',
    body: '480fafc2',
    instructions: 1,
    factor: 5,
    flagMask: 0xc03n,
    setup: (s) => {
      s.gpr[0] = 1n;
      s.gpr[2] = 1n;
    },
    expected: (s) => {
      s.flags = 0x402n;
    },
  },
  {
    name: 'unsigned-divide',
    body: 'b86400000031d248f7f7',
    instructions: 3,
    factor: 5,
    flagMask: 0x402n,
    setup: (s) => {
      s.gpr[7] = 7n;
    },
    expected: (s) => {
      s.gpr[0] = 14n;
      s.gpr[2] = 2n;
      s.flags = 0x402n;
    },
  },
  {
    name: 'integer-qword-add',
    body: '660fd4c1',
    instructions: 1,
    setup: (s) => {
      s.xmm[1] = pack([7n, 11n], 64);
    },
    expected: (s, n) => {
      s.xmm[0] =
        (((s.xmm[0] & MASK64) + 7n * n) & MASK64) |
        ((((s.xmm[0] >> 64n) + 11n * n) & MASK64) << 64n);
    },
  },
  {
    name: 'checked-vector-load',
    body: 'f30f6f03',
    instructions: 1,
    expected: (s) => {
      s.xmm[0] = read(s.ram, 16, 16);
    },
  },
  {
    name: 'checked-vector-store-unaligned',
    body: 'f30f7f03',
    instructions: 1,
    setup: (s) => {
      s.gpr[3] = RAM + 17n;
    },
    expected: (s) => little(s.ram, 17, s.xmm[0], 16),
  },
  {
    name: 'scalar-f32-add-inexact',
    body: '660f6fc2f30f58c1',
    instructions: 2,
    setup: (s) => {
      s.xmm[2] = low32(s.xmm[2], 0x3f800000);
      s.xmm[1] = low32(s.xmm[1], 0x33800000);
    },
    expected: (s) => {
      s.xmm[0] = s.xmm[2];
      s.mxcsr = 0x1fa0;
    },
  },
  {
    name: 'packed-f32-add-exact',
    body: '660f6fc20f58c1',
    instructions: 2,
    setup: (s) => {
      s.xmm[2] = pack([0x3f800000, 0x40000000, 0x40800000, 0x41000000], 32);
      s.xmm[1] = pack(Array(4).fill(0x3f000000), 32);
    },
    expected: (s) => {
      s.xmm[0] = pack([0x3fc00000, 0x40200000, 0x40900000, 0x41080000], 32);
    },
  },
  {
    name: 'scalar-f64-sqrt',
    body: 'f20f51c1',
    instructions: 1,
    setup: (s) => {
      s.xmm[1] = low64(s.xmm[1], 0x4000000000000000n);
    },
    expected: (s) => {
      s.xmm[0] = low64(s.xmm[0], 0x3ff6a09e667f3bcdn);
      s.mxcsr = 0x1fa0;
    },
  },
  {
    name: 'signed-i64-to-f64',
    body: 'f2480f2ac2',
    instructions: 1,
    setup: (s) => {
      s.gpr[2] = 0x0020000000000001n;
    },
    expected: (s) => {
      s.xmm[0] = low64(s.xmm[0], 0x4340000000000000n);
      s.mxcsr = 0x1fa0;
    },
  },
  {
    name: 'memory-f32-to-gpr',
    body: 'f30f2c03',
    instructions: 1,
    setup: (s) => little(s.ram, 16, 0xc0e00000n, 4),
    expected: (s) => {
      s.gpr[0] = 0xfffffff9n;
    },
  },
  {
    name: 'packed-f32-reciprocal',
    body: '0f53c1',
    instructions: 1,
    setup: (s) => {
      s.xmm[1] = pack([0x3f800000, 0x40000000, 0x40800000, 0x41000000], 32);
      s.mxcsr = 0xffff;
    },
    expected: (s) => {
      s.xmm[0] = pack([0x3f800000, 0x3f000000, 0x3e800000, 0x3e000000], 32);
    },
  },
  {
    name: 'mxcsr-reset-each-iteration',
    body: '0fae13660f6fc2f30f58c1',
    instructions: 3,
    setup: (s) => {
      little(s.ram, 16, 0x5f80n, 4);
      s.xmm[2] = low32(s.xmm[2], 0x3f800000);
      s.xmm[1] = low32(s.xmm[1], 0x33800000);
    },
    expected: (s) => {
      s.xmm[0] = low32(s.xmm[2], 0x3f800001);
      s.mxcsr = 0x5fa0;
    },
  },
  {
    name: 'budget-one-resume',
    body: '4883c001',
    instructions: 1,
    resume: true,
    setup: (s) => {
      s.gpr[0] = 0n;
    },
    expected: (s, n) => {
      s.gpr[0] = n;
      s.flags = addFlags(n);
    },
  },
  {
    name: 'rep-forward-copy',
    repeat: true,
    body: 'f3a4',
    instructions: 1,
    expected: (s, n) => {
      const size = Number(n);
      s.ram.copyWithin(size + 32, 0, size);
      s.gpr[6] = RAM + n;
      s.gpr[7] = RAM + 2n * n + 32n;
    },
  },
];
