import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import os from 'node:os';

function commandExists(command) {
  const lookup = os.platform() === 'win32' ? 'where' : 'which';
  try {
    execFileSync(lookup, [command], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

function log(message) {
  process.stderr.write(`${message}\n`);
}

function detectGpuFeature() {
  const platform = os.platform();

  if (platform !== 'win32') {
    log('Meetly Lite GPU auto-detection is currently Windows-focused.');
    return '';
  }

  if (commandExists('nvidia-smi')) {
    if (process.env.CUDA_PATH || commandExists('nvcc')) {
      log('NVIDIA GPU and CUDA tooling detected. Using CUDA acceleration.');
      return 'cuda';
    }
    log('NVIDIA GPU detected, but CUDA Toolkit was not found.');
  }

  if (process.env.VULKAN_SDK && (commandExists('vulkaninfo') || existsSync(process.env.VULKAN_SDK))) {
    log('Vulkan SDK detected. Using Vulkan acceleration.');
    return 'vulkan';
  }

  if (commandExists('vulkaninfo')) {
    log('Vulkan capability was detected, but VULKAN_SDK is not set.');
  }

  log('No GPU acceleration backend detected. Using default CPU build.');
  return '';
}

const feature = detectGpuFeature();
if (feature) {
  process.stdout.write(feature);
}