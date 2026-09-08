import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';

import { REQUIRED_DLLS, findFoundryNativeDirectory, stageNativeDlls } from './stage-foundry-native.mjs';

async function createSdkOutput(root, hash, files) {
  const output = join(root, 'release', 'build', `foundry-local-sdk-${hash}`, 'out');
  await mkdir(output, { recursive: true });
  await Promise.all(files.map((file) => writeFile(join(output, file), file)));
  return output;
}

test('stages all DLLs from a complete SDK output and removes stale files', async (context) => {
  const root = await mkdtemp(join(process.cwd(), '.meetly-foundry-stage-'));
  context.after(() => rm(root, { recursive: true, force: true }));
  const stage = join(root, 'stage');
  await createSdkOutput(root, 'current', [...REQUIRED_DLLS, 'auxiliary.dll']);
  await mkdir(stage, { recursive: true });
  await writeFile(join(stage, '.gitignore'), '*\n!.gitignore\n');
  await writeFile(join(stage, 'stale.dll'), 'stale');

  const result = await stageNativeDlls({ targetDirectory: root, stageDirectory: stage });

  assert.equal(result.nativeDlls.length, REQUIRED_DLLS.length + 1);
  assert.deepEqual(
    (await readdir(stage)).sort(),
    ['.gitignore', ...REQUIRED_DLLS, 'auxiliary.dll'].sort(),
  );
});

test('rejects SDK outputs missing a mandatory DLL', async (context) => {
  const root = await mkdtemp(join(process.cwd(), '.meetly-foundry-stage-'));
  context.after(() => rm(root, { recursive: true, force: true }));
  await createSdkOutput(root, 'incomplete', REQUIRED_DLLS.slice(1));

  await assert.rejects(
    () => findFoundryNativeDirectory(root),
    /No foundry-local-sdk output contains all required DLLs/,
  );
});
