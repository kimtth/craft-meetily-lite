import { cp, mkdir, readdir, rm, stat } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

export const REQUIRED_DLLS = [
  'Microsoft.AI.Foundry.Local.Core.dll',
  'onnxruntime.dll',
  'onnxruntime-genai.dll',
  'Microsoft.Windows.AI.MachineLearning.dll',
];

function hasRequiredDlls(entries) {
  const names = new Set(entries.map((entry) => entry.toLowerCase()));
  return REQUIRED_DLLS.every((name) => names.has(name.toLowerCase()));
}

export async function findFoundryNativeDirectory(targetDirectory) {
  const buildDirectory = join(targetDirectory, 'release', 'build');
  if (!existsSync(buildDirectory)) {
    throw new Error(
      `Foundry native output was not found at ${buildDirectory}. Run the release Cargo build first.`,
    );
  }

  const candidates = await Promise.all(
    (await readdir(buildDirectory, { withFileTypes: true }))
      .filter((entry) => entry.isDirectory() && entry.name.startsWith('foundry-local-sdk-'))
      .map(async (entry) => {
        const outputDirectory = join(buildDirectory, entry.name, 'out');
        if (!existsSync(outputDirectory)) {
          return null;
        }
        const entries = await readdir(outputDirectory, { withFileTypes: true });
        const files = entries.filter((item) => item.isFile()).map((item) => item.name);
        if (!hasRequiredDlls(files)) {
          return null;
        }
        return {
          directory: outputDirectory,
          modifiedAt: (await stat(outputDirectory)).mtimeMs,
        };
      }),
  );

  const selected = candidates
    .filter((candidate) => candidate !== null)
    .sort((left, right) => right.modifiedAt - left.modifiedAt)[0];
  if (!selected) {
    throw new Error(
      `No foundry-local-sdk output contains all required DLLs: ${REQUIRED_DLLS.join(', ')}.`,
    );
  }
  return selected.directory;
}

export async function stageNativeDlls({ targetDirectory, stageDirectory }) {
  const sourceDirectory = await findFoundryNativeDirectory(targetDirectory);
  const sourceFiles = await readdir(sourceDirectory, { withFileTypes: true });
  const nativeDlls = sourceFiles
    .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith('.dll'))
    .map((entry) => entry.name);

  if (!hasRequiredDlls(nativeDlls)) {
    throw new Error(`Required Foundry DLLs are missing from ${sourceDirectory}.`);
  }

  await mkdir(stageDirectory, { recursive: true });
  await Promise.all(
    (await readdir(stageDirectory, { withFileTypes: true }))
      .filter((entry) => entry.name !== '.gitignore')
      .map((entry) => rm(join(stageDirectory, entry.name), { recursive: true, force: true })),
  );
  await Promise.all(
    nativeDlls.map((file) => cp(join(sourceDirectory, file), join(stageDirectory, file))),
  );
  return { sourceDirectory, nativeDlls };
}

function runCargoReleaseBuild(repositoryRoot) {
  const result = spawnSync(
    'cargo',
    ['build', '--manifest-path', join(repositoryRoot, 'src-tauri', 'Cargo.toml'), '--release'],
    { cwd: repositoryRoot, stdio: 'inherit', shell: process.platform === 'win32' },
  );
  if (result.status !== 0) {
    throw new Error('Release Cargo build failed; Foundry native DLLs were not staged.');
  }
}

async function main() {
  const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const targetDirectory = resolve(
    process.env.CARGO_TARGET_DIR ?? join(repositoryRoot, 'src-tauri', 'target'),
  );
  const stageDirectory = join(repositoryRoot, 'src-tauri', 'resources', 'foundry-native');

  runCargoReleaseBuild(repositoryRoot);
  const { sourceDirectory, nativeDlls } = await stageNativeDlls({
    targetDirectory,
    stageDirectory,
  });
  console.log(`Staged ${nativeDlls.length} Foundry native DLLs from ${sourceDirectory}:`);
  for (const dll of nativeDlls) {
    console.log(`  ${dll}`);
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(`Failed to stage Foundry native DLLs: ${error.message}`);
    process.exitCode = 1;
  });
}
