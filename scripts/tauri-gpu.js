import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';

const command = process.argv[2];
const forcedFeature = process.argv[3];

if (!['dev', 'build'].includes(command)) {
  process.stderr.write('Usage: node scripts/tauri-gpu.js <dev|build> [cuda|vulkan|openblas]\n');
  process.exit(1);
}

if (forcedFeature && !['cuda', 'vulkan', 'openblas'].includes(forcedFeature)) {
  process.stderr.write(`Unsupported GPU feature: ${forcedFeature}\n`);
  process.exit(1);
}

function detectFeature() {
  if (forcedFeature) {
    return forcedFeature;
  }

  if (process.env.TAURI_GPU_FEATURE) {
    return process.env.TAURI_GPU_FEATURE.trim();
  }

  try {
    return execFileSync(process.execPath, ['scripts/auto-detect-gpu.js'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'inherit'],
    }).trim();
  } catch {
    return '';
  }
}

const feature = detectFeature();
const args = [command];
const env = { ...process.env };

function findBundledNinja() {
  if (process.platform !== 'win32') {
    return '';
  }

  const candidates = [
    'C:\\Program Files (x86)\\Microsoft Visual Studio\\2022\\BuildTools\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\Ninja\\ninja.exe',
    'C:\\Program Files\\Microsoft Visual Studio\\2022\\BuildTools\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\Ninja\\ninja.exe',
    'C:\\Program Files\\CMake\\bin\\ninja.exe',
  ];

  return candidates.find((candidate) => existsSync(candidate)) ?? '';
}

if (feature && !env.CARGO_TARGET_DIR) {
  env.CARGO_TARGET_DIR = path.join(path.parse(process.cwd()).root, 'mtg');
  process.stderr.write(`Using short Cargo target directory: ${env.CARGO_TARGET_DIR}\n`);
}

if (feature) {
  const ninja = findBundledNinja();
  if (ninja) {
    env.CMAKE_GENERATOR = env.CMAKE_GENERATOR || 'Ninja';
    env.CMAKE_MAKE_PROGRAM = env.CMAKE_MAKE_PROGRAM || ninja;
    const pathKey = Object.keys(env).find((key) => key.toLowerCase() === 'path') ?? 'PATH';
    env[pathKey] = `${path.dirname(ninja)}${path.delimiter}${env[pathKey] ?? ''}`;
    process.stderr.write(`Using CMake generator: ${env.CMAKE_GENERATOR}\n`);
    process.stderr.write(`Using Ninja: ${ninja}\n`);
  }
  env.CARGO_BUILD_JOBS = env.CARGO_BUILD_JOBS || '1';
  env.CMAKE_BUILD_PARALLEL_LEVEL = env.CMAKE_BUILD_PARALLEL_LEVEL || '1';
  args.push('--', '--features', feature, '--jobs', '1');
  process.stderr.write(`Running Tauri ${command} with Whisper feature: ${feature}\n`);
} else {
  process.stderr.write(`Running Tauri ${command} with default CPU Whisper backend.\n`);
}

const result = spawnSync('tauri', args, {
  stdio: 'inherit',
  shell: process.platform === 'win32',
  env,
});

process.exit(result.status ?? 1);