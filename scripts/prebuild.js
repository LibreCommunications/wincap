#!/usr/bin/env node
// Driver for prebuildify across Node + Electron ABIs.
// LibreCord ships on Electron 41.1 — that ABI is mandatory in the matrix.
'use strict';

const { spawnSync } = require('node:child_process');

// wincap exists to power LibreCord. We only ship the ABI LibreCord
// actually uses; add more entries here when LibreCord moves.
const TARGETS = [
  { runtime: 'electron', target: '41.1.1' },
];

const ARCHES = ['x64'];

for (const arch of ARCHES) {
  for (const t of TARGETS) {
    const args = [
      'prebuildify',
      '--napi',
      '--strip',
      '--arch', arch,
      '--target', `${t.runtime}@${t.target}`,
    ];
    console.log('>', 'npx', args.join(' '));
    const r = spawnSync('npx', args, { stdio: 'inherit', shell: true });
    if (r.status !== 0) process.exit(r.status ?? 1);
  }
}
