// resticker — чистые функции работы с хоткеями (разбор кодов клавиш,
// проверка модификаторов, сборка комбинации).
//
// Вынесено отдельно от main.js, чтобы чистая логика была изолирована от
// DOM-эффектов и полностью покрывалась автоматическими юнит-тестами
// (node --test hotkey_test.js).

/**
 * Проверяет, является ли клавиша модификатором (Ctrl, Alt, Shift, Win/Meta).
 *
 * При нажатии модификатора поле ввода не должно завершать запись, а должно
 * показывать живой превью зажатых клавиш ("Alt + Shift + …").
 */
function isModifierKey(code, key) {
  if ([
    'AltLeft', 'AltRight',
    'ControlLeft', 'ControlRight',
    'ShiftLeft', 'ShiftRight',
    'MetaLeft', 'MetaRight',
    'OSLeft', 'OSRight',
  ].includes(code)) {
    return true;
  }
  if (['Alt', 'Control', 'Shift', 'Meta'].includes(key)) {
    return true;
  }
  return false;
}

/**
 * Преобразует физический код клавиши (KeyboardEvent.code) или имя клавиши
 * (KeyboardEvent.key) в каноничный токен для HotkeyCombo в rst-win32.
 *
 * Поддерживаются:
 * - Буквы: 'KeyA'..'KeyZ' -> 'A'..'Z'
 * - Цифры: 'Digit0'..'Digit9', 'Numpad0'..'Numpad9' -> '0'..'9'
 * - Функциональные клавиши: 'F1'..'F24' -> 'F1'..'F24'
 *
 * Раскладко-независимо: KeyboardEvent.code отражает физическую клавишу на
 * клавиатуре (на русской раскладке клавиша 'S' имеет код 'KeyS').
 */
function codeToKeyToken(code, key) {
  if (typeof code === 'string') {
    if (code.startsWith('Key') && code.length === 4) return code.slice(3).toUpperCase();
    if (code.startsWith('Digit') && code.length === 6) return code.slice(5);
    if (code.startsWith('Numpad') && /^Numpad[0-9]$/.test(code)) return code.slice(6);
    if (/^F([1-9]|1\d|2[0-4])$/i.test(code)) return code.toUpperCase();
  }
  if (typeof key === 'string' && key.length === 1) {
    const ch = key.toUpperCase();
    if ((ch >= 'A' && ch <= 'Z') || (ch >= '0' && ch <= '9')) return ch;
  }
  return null;
}

/**
 * Извлекает список зажатых модификаторов из KeyboardEvent в каноничном
 * порядке: Ctrl, Alt, Shift, Win.
 */
function getModifiersFromEvent(e) {
  const parts = [];
  if (e.ctrlKey || e.code === 'ControlLeft' || e.code === 'ControlRight') parts.push('Ctrl');
  if (e.altKey || e.code === 'AltLeft' || e.code === 'AltRight') parts.push('Alt');
  if (e.shiftKey || e.code === 'ShiftLeft' || e.code === 'ShiftRight') parts.push('Shift');
  if (e.metaKey || e.code === 'MetaLeft' || e.code === 'MetaRight') parts.push('Win');
  return parts;
}

/**
 * Собирает итоговую строку комбинации (например "Alt+Shift+S").
 *
 * Возвращает `{ success: true, combo }` или `{ success: false, error }`:
 * - 'modifier_required': если пользователь нажал клавишу без модификаторов
 * - 'key_required': если основная клавиша не передана
 */
function buildHotkeyCombo(modifiers, keyToken) {
  if (!modifiers || modifiers.length === 0) {
    return { success: false, error: 'modifier_required' };
  }
  if (!keyToken) {
    return { success: false, error: 'key_required' };
  }
  return { success: true, combo: [...modifiers, keyToken].join('+') };
}

/**
 * Форматирует строку живого превью зажатых модификаторов ("Alt + Shift + …").
 */
function formatHoldingModifiers(modifiers, placeholderText = 'Press a combination…') {
  if (!modifiers || modifiers.length === 0) return placeholderText;
  return modifiers.join(' + ') + ' + …';
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    isModifierKey,
    codeToKeyToken,
    getModifiersFromEvent,
    buildHotkeyCombo,
    formatHoldingModifiers,
  };
}
if (typeof window !== 'undefined') {
  window.isModifierKey = isModifierKey;
  window.codeToKeyToken = codeToKeyToken;
  window.getModifiersFromEvent = getModifiersFromEvent;
  window.buildHotkeyCombo = buildHotkeyCombo;
  window.formatHoldingModifiers = formatHoldingModifiers;
}
