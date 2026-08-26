// Тесты чистой логики захвата хоткеев (Node.js test runner: `node --test hotkey_test.js`).
const test = require('node:test');
const assert = require('node:assert/strict');
const {
  isModifierKey,
  codeToKeyToken,
  getModifiersFromEvent,
  buildHotkeyCombo,
  formatHoldingModifiers,
} = require('./hotkey_util.js');

test('codeToKeyToken преобразует KeyS и KeyF в S и F', () => {
  assert.equal(codeToKeyToken('KeyS'), 'S');
  assert.equal(codeToKeyToken('KeyF'), 'F');
  assert.equal(codeToKeyToken('KeyA'), 'A');
  assert.equal(codeToKeyToken('KeyZ'), 'Z');
});

test('codeToKeyToken преобразует Digit0..9 и Numpad0..9 в цифры', () => {
  assert.equal(codeToKeyToken('Digit0'), '0');
  assert.equal(codeToKeyToken('Digit9'), '9');
  assert.equal(codeToKeyToken('Numpad5'), '5');
});

test('codeToKeyToken преобразует F1..F24', () => {
  assert.equal(codeToKeyToken('F1'), 'F1');
  assert.equal(codeToKeyToken('F12'), 'F12');
  assert.equal(codeToKeyToken('F24'), 'F24');
  assert.equal(codeToKeyToken('F25'), null);
});

test('codeToKeyToken возвращает null для неподдерживаемых клавиш', () => {
  assert.equal(codeToKeyToken('Space'), null);
  assert.equal(codeToKeyToken('Tab'), null);
  assert.equal(codeToKeyToken('ArrowUp'), null);
  assert.equal(codeToKeyToken('Escape'), null);
  assert.equal(codeToKeyToken('Enter'), null);
});

test('isModifierKey распознаёт все клавиши модификаторов', () => {
  assert.equal(isModifierKey('AltLeft'), true);
  assert.equal(isModifierKey('AltRight'), true);
  assert.equal(isModifierKey('ControlLeft'), true);
  assert.equal(isModifierKey('ControlRight'), true);
  assert.equal(isModifierKey('ShiftLeft'), true);
  assert.equal(isModifierKey('ShiftRight'), true);
  assert.equal(isModifierKey('MetaLeft'), true);
  assert.equal(isModifierKey('KeyS'), false);
  assert.equal(isModifierKey('Space'), false);
});

test('getModifiersFromEvent извлекает модификаторы в каноничном порядке Ctrl, Alt, Shift, Win', () => {
  const e1 = { ctrlKey: false, altKey: true, shiftKey: true, metaKey: false };
  assert.deepEqual(getModifiersFromEvent(e1), ['Alt', 'Shift']);

  const e2 = { ctrlKey: true, altKey: true, shiftKey: true, metaKey: true };
  assert.deepEqual(getModifiersFromEvent(e2), ['Ctrl', 'Alt', 'Shift', 'Win']);

  const e3 = { ctrlKey: true, altKey: false, shiftKey: false, metaKey: false };
  assert.deepEqual(getModifiersFromEvent(e3), ['Ctrl']);
});

test('buildHotkeyCombo собирает Alt+Shift+S и Alt+Shift+F', () => {
  const resS = buildHotkeyCombo(['Alt', 'Shift'], 'S');
  assert.equal(resS.success, true);
  assert.equal(resS.combo, 'Alt+Shift+S');

  const resF = buildHotkeyCombo(['Alt', 'Shift'], 'F');
  assert.equal(resF.success, true);
  assert.equal(resF.combo, 'Alt+Shift+F');
});

test('buildHotkeyCombo возвращает modifier_required при отсутствии модификаторов', () => {
  const res = buildHotkeyCombo([], 'S');
  assert.equal(res.success, false);
  assert.equal(res.error, 'modifier_required');
});

test('buildHotkeyCombo возвращает key_required при отсутствии основной клавиши', () => {
  const res = buildHotkeyCombo(['Ctrl', 'Alt'], null);
  assert.equal(res.success, false);
  assert.equal(res.error, 'key_required');
});

test('formatHoldingModifiers формирует превью зажатых модификаторов', () => {
  assert.equal(formatHoldingModifiers(['Alt']), 'Alt + …');
  assert.equal(formatHoldingModifiers(['Alt', 'Shift']), 'Alt + Shift + …');
  assert.equal(formatHoldingModifiers(['Ctrl', 'Alt', 'Shift']), 'Ctrl + Alt + Shift + …');
});
