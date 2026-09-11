import assert from 'node:assert/strict';

export const navigationScenarios = [
  'settings-select-current',
  'settings-select-different',
  'idle-navigation-held-runtime-status',
];

// Real DOM only: the meeting's heading is .meeting-header with an editable
// "Meeting title" input, NOT an h1. A selected sidebar card alone proves nothing
// about which workspace is visible. Do not call/extract App handlers or set state.
async function observe(page) {
  const header = page.locator('.meeting-workspace .meeting-header');
  const title = header.getByRole('textbox', { name: 'Meeting title', exact: true });
  return {
    workspaceVisible: await page.locator('.meeting-workspace').isVisible(),
    headingVisible: await header.isVisible(),
    titleVisible: await title.isVisible(),
    title: await title.count() ? await title.inputValue() : null,
    settingsVisible: await page.getByRole('heading', { name: 'Settings', exact: true }).isVisible(),
    selectedCards: await page.locator('.meeting-card.selected > span').allTextContents(),
    recordingState: await page.locator('.recording-state strong').innerText(),
    stopDialogs: await page.getByRole('alertdialog').count(),
    fixture: await page.evaluate(() => {
      const s = window.__recordingE2E;
      return {
        activeId: s.activeId, runtimeStatusPending: s.runtimeStatusPending,
        runtimeStatusResolved: s.runtimeStatusResolved, runtimeStatusHeld: s.runtimeStatusHeld,
        captureCalls: s.calls.filter((call) => /^(start|stop)_recording$/.test(call.command)),
        failures: s.failures, speechSessions: s.speech.length,
      };
    }),
  };
}

async function waitForWorkspace(page, title) {
  // Bounded responsiveness contract; a held IPC response is NEVER released to
  // make this assertion pass. Poll DOM, not timing or private React internals.
  try {
    await page.waitForFunction((wanted) => {
      const input = document.querySelector('.meeting-workspace .meeting-header input[aria-label="Meeting title"]');
      const selected = [...document.querySelectorAll('.meeting-card.selected > span')];
      const settings = [...document.querySelectorAll('h2')].find((node) => node.textContent === 'Settings');
      return input?.checkVisibility() && input.value === wanted
        && selected.length === 1 && selected[0].textContent === wanted
        && !settings?.checkVisibility();
    }, title, { timeout: 2000 });
  } catch (error) {
    if (error.name !== 'TimeoutError') throw error;
    // Collect EVERY failed UI expectation below, not just the first timeout.
  }
}

export async function runNavigationScenario({ page, item, snapshot }) {
  const a = 'Synthetic Wire Session A';
  const b = 'Synthetic Wire Session B';
  const held = item.name === 'idle-navigation-held-runtime-status';
  const requested = item.name === 'settings-select-current' ? a : b;
  const failures = [];
  item.assertionResults = [];
  item.navigationDeadlineMs = 2000;
  const check = (name, fn) => {
    try {
      fn();
      item.assertions.push(name);
      item.assertionResults.push({ name, status: 'passed' });
    } catch (error) {
      failures.push(name);
      item.assertionResults.push({ name, status: 'failed', error: error.message });
    }
  };
  const assertWorkspace = (label, actual, title) => {
    check(`${label}: actual meeting workspace heading/title visible for ${title}`, () => {
      assert.deepEqual(
        { workspace: actual.workspaceVisible, heading: actual.headingVisible, titleVisible: actual.titleVisible, title: actual.title },
        { workspace: true, heading: true, titleVisible: true, title },
      );
    });
    check(`${label}: Settings is not visible`, () => assert.equal(actual.settingsVisible, false));
    check(`${label}: only requested card is selected`, () => assert.deepEqual(actual.selectedCards, [title]));
    check(`${label}: idle Ready, no stop warning, capture, stopping or recognizer`, () => {
      assert.equal(actual.recordingState, 'Ready');
      assert.equal(actual.stopDialogs, 0);
      assert.equal(actual.fixture.activeId, null);
      assert.deepEqual(actual.fixture.captureCalls, []);
      assert.equal(actual.fixture.speechSessions, 0);
    });
  };
  const card = (title) => page.locator('.meeting-cards').getByRole('button', { name: new RegExp(`^${title}`) });

  await page.waitForFunction(() => window.__recordingE2E?.settingsWaiting);
  await page.evaluate(() => { window.__recordingE2E.releaseSettings(); window.__recordingE2E.releaseCatalog(); });
  // Catalog invocation follows startup runtime reconciliation. Unlike the old
  // delayed-settings tests, these scenarios never race that reconciliation.
  await page.waitForFunction(() => {
    const s = window.__recordingE2E;
    return s.catalogWaiting && s.catalogResolved && s.runtimeStatusResolved > 0 && s.runtimeStatusPending === 0;
  });
  await page.locator('.recording-state strong').filter({ hasText: /^Ready$/ }).waitFor();
  if (!await page.locator('.meeting-list').isVisible()) await page.getByTitle('Toggle recordings', { exact: true }).click();
  await card(a).click();
  await waitForWorkspace(page, a);
  item.before = await observe(page);
  assertWorkspace('Idle startup baseline', item.before, a);
  assert.equal(failures.length, 0, `Invalid navigation precondition: ${failures.join('; ')}`);
  try {
    if (held) {
      await page.evaluate(() => window.__recordingE2E.holdRuntimeStatus());
    } else {
      // Start with the list closed so the requested Toggle always OPENS it.
      await page.getByTitle('Toggle recordings', { exact: true }).click();
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await page.getByRole('heading', { name: 'Settings', exact: true }).waitFor();
      await page.getByTitle('Toggle recordings', { exact: true }).click();
      await page.locator('.meeting-list').waitFor();
      item.afterToggle = await observe(page);
    }
    await card(requested).click();
    await waitForWorkspace(page, requested);
    // Capture evidence while the response is STILL held / Settings still open.
    item.requestedTitle = requested;
    item.afterClick = await observe(page);
    assertWorkspace(held ? 'While runtime status is held' : 'Settings → Toggle recordings → card', item.afterClick, requested);
    await snapshot('after-card-click');
  } finally {
    if (held) await page.evaluate(() => window.__recordingE2E.releaseRuntimeStatus());
  }
  if (held) {
    await page.waitForFunction(() => window.__recordingE2E.runtimeStatusPending === 0);
    await waitForWorkspace(page, b);
    item.afterRelease = await observe(page);
    assertWorkspace('After releasing runtime status (diagnostic control)', item.afterRelease, b);
    await card(a).click();
    await waitForWorkspace(page, a);
    item.returnNavigation = await observe(page);
    assertWorkspace('Return navigation control', item.returnNavigation, a);
  }
  const state = await page.evaluate(() => ({ calls: window.__recordingE2E.calls, log: window.__recordingE2E.log, failures: window.__recordingE2E.failures }));
  item.nativeCalls = state.calls;
  item.lifecycle = state.log;
  check('No unknown IPC or uncaught browser errors', () => {
    assert.deepEqual(state.failures, []);
    assert.deepEqual(item.pageErrors, []);
    assert.deepEqual(item.consoleErrors.filter((text) => !/Content Security Policy|content security policy|Failed to load resource.*ERR_BLOCKED_BY_CLIENT/.test(text)), []);
  });
  check('No external HTTP/WebSocket request escaped the offline boundary', () => assert.deepEqual(item.blocked, []));
  const errorBanners = await page.locator('.error-banner').count();
  check('No UI error banner', () => assert.equal(errorBanners, 0));
  if (failures.length) throw new Error(`${item.name}:\n- ${failures.join('\n- ')}`);
}