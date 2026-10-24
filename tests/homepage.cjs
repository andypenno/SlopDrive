const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { runInNewContext } = require('node:vm');

const html = readFileSync(resolve(__dirname, '../static/index.html'), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const json = html.match(/<code id="config-json">([\s\S]*?)<\/code>/)[1];
assert.equal(JSON.parse(json).mcpServers.slopdrive.headers.Authorization, 'Bearer YOUR_API_KEY');

async function checkCopy(clipboard, succeeds) {
  let click;
  let selected;
  let focused = false;
  let cleared = false;
  let added = false;
  const button = { disabled: false, addEventListener: (event, handler) => {
    assert.equal(event, 'click');
    click = handler;
  } };
  const config = { textContent: json };
  const status = { textContent: 'Previous result' };
  const range = { selectNodeContents: node => { selected = node; } };
  runInNewContext(script, {
    navigator: { clipboard },
    document: {
      getElementById: id => ({
        'copy-config': button, 'config-json': config, 'copy-status': status,
        'config-block': { focus: () => { focused = true; } },
      })[id],
      createRange: () => range,
    },
    window: { getSelection: () => ({
      removeAllRanges: () => { cleared = true; },
      addRange: value => { assert.equal(value, range); added = true; },
    }) },
  });
  const pending = click();
  if (succeeds) {
    assert.equal(button.disabled, true);
    assert.equal(status.textContent, '');
  }
  await pending;
  assert.equal(button.disabled, false);
  if (succeeds) {
    assert.match(status.textContent, /Configuration copied/);
    assert.equal(focused, false);
    assert.equal(selected, undefined);
  } else {
    assert.match(status.textContent, /Ctrl\+C or Command\+C/);
    assert.equal(focused, true);
    assert.equal(selected, config);
    assert.equal(cleared, true);
    assert.equal(added, true);
  }
}

(async () => {
  let copied;
  await checkCopy({ writeText: async text => { copied = text; } }, true);
  assert.equal(copied, json);
  await checkCopy({ writeText: async () => { throw new Error('Permission denied'); } }, false);
  await checkCopy(undefined, false);
  console.log('Homepage copy checks passed for success, denial and unavailable clipboard');
})().catch(error => { console.error(error); process.exitCode = 1; });
