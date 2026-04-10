#!/usr/bin/env node
// Driver for prebuildify across Node + Electron ABIs.
// LibreCord ships on Electron 41.1 — that ABI is mandatory in the matrix.
'use strict';

const { spawnSync } = require('node:child_process');

const TARGETS = [
  { runtime: 'node', target: '20.18.0' },
  { runtime: 'node', target: '22.11.0' },
  { runtime: 'electron', target: '41.1.1' }, // LibreCord
  { runtime: 'electron', target: '34.0.0' },
];

const ARCHES = ['x64', 'arm64'];

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
