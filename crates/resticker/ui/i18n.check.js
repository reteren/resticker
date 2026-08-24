// Автономная проверка таблицы строк окна настроек — без фреймворка, тот же
// принцип «no runtime JS framework», что у main.js. Запуск:
// `node crates/resticker/ui/i18n.check.js` (или `npm run test:i18n` из корня
// репозитория).
//
// Проверяет: (1) любой ключ, реально используемый в HTML (`data-i18n*`) или
// JS (`t('key'`), есть в таблице; (2) в таблице нет ключей, которых не
// использует никто — забытая строка после правки разметки; (3) значения
// непустые. Язык один (см. доккомент i18n.js), поэтому сверять пару
// словарей больше не нужно.

const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert');

const dir = __dirname;
const i18nSrc = fs.readFileSync(path.join(dir, 'i18n.js'), 'utf8');
const mainSrc = fs.readFileSync(path.join(dir, 'main.js'), 'utf8');
const htmlSrc = fs.readFileSync(path.join(dir, 'index.html'), 'utf8');

// Загружаем таблицу реальным выполнением i18n.js в изолированном контексте —
// надёжнее, чем парсить объектный литерал регэкспом.
const vm = require('node:vm');
const sandbox = { document: { documentElement: {}, querySelectorAll: () => [] } };
vm.createContext(sandbox);
vm.runInContext(i18nSrc + '\nthis.__STRINGS = STRINGS;', sandbox);
const STRINGS = sandbox.__STRINGS;

assert.ok(STRINGS && typeof STRINGS === 'object', 'i18n.js должен экспортировать STRINGS');

const keys = new Set(Object.keys(STRINGS));

const empty = [...keys].filter((k) => !String(STRINGS[k]).trim());
assert.deepStrictEqual(empty, [], `пустые строки: ${empty.join(', ')}`);

// Ключи, реально используемые в коде — t('key' / t("key" / data-i18n="key".
const usedKeys = new Set();
for (const m of mainSrc.matchAll(/\bt\(\s*['"]([\w.]+)['"]/g)) usedKeys.add(m[1]);
for (const m of htmlSrc.matchAll(/data-i18n(?:-\w+)?="([\w.]+)"/g)) usedKeys.add(m[1]);

assert.ok(usedKeys.size > 30, `подозрительно мало найденных ключей: ${usedKeys.size}`);

const missing = [...usedKeys].filter((k) => !keys.has(k));
assert.deepStrictEqual(missing, [], `используемые ключи без строки: ${missing.join(', ')}`);

// Ключи, которые зовутся не литералом: множественное число из
// pluralStickerWord() и заголовок окна из applyStaticTranslations() —
// исключаем их из проверки «мёртвых» строк.
const IMPLICIT = new Set([
  'preset.word.one',
  'preset.word.many',
  'window.title',
]);
const unused = [...keys].filter((k) => !usedKeys.has(k) && !IMPLICIT.has(k));
assert.deepStrictEqual(unused, [], `строки, которых никто не использует: ${unused.join(', ')}`);

console.log(
  `i18n.check.js: OK — ${keys.size} строк в таблице, ${usedKeys.size} ключей используются в коде.`
);
