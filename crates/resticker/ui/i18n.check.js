// Автономная проверка словаря локализации (ROADMAP.md M8, «локализация:
// русский и английский») — без фреймворка, тот же принцип «no runtime JS
// framework», что у main.js. Запуск: `node crates/resticker/ui/i18n.check.js`
// (или `npm run test:i18n` из корня репозитория).
//
// Проверяет: (1) любой ключ, реально используемый в HTML (`data-i18n*`) или
// JS (`t('key'`), есть в обоих словарях; (2) `ru` и `en` не разошлись по
// набору ключей (не забыли добавить перевод при добавлении ключа).

const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert');

const dir = __dirname;
const i18nSrc = fs.readFileSync(path.join(dir, 'i18n.js'), 'utf8');
const mainSrc = fs.readFileSync(path.join(dir, 'main.js'), 'utf8');
const htmlSrc = fs.readFileSync(path.join(dir, 'index.html'), 'utf8');

// Загружаем словарь реальным выполнением i18n.js в изолированном контексте —
// надёжнее, чем парсить объектный литерал регэкспом.
const vm = require('node:vm');
const sandbox = { document: { documentElement: {}, querySelectorAll: () => [] } };
vm.createContext(sandbox);
vm.runInContext(i18nSrc + '\nthis.__DICT = DICT;', sandbox);
const DICT = sandbox.__DICT;

assert.ok(DICT.ru && DICT.en, 'словарь должен содержать ru и en');

const ruKeys = new Set(Object.keys(DICT.ru));
const enKeys = new Set(Object.keys(DICT.en));

const onlyInRu = [...ruKeys].filter((k) => !enKeys.has(k));
const onlyInEn = [...enKeys].filter((k) => !ruKeys.has(k));
assert.deepStrictEqual(onlyInRu, [], `ключи есть в ru, но нет в en: ${onlyInRu.join(', ')}`);
assert.deepStrictEqual(onlyInEn, [], `ключи есть в en, но нет в ru: ${onlyInEn.join(', ')}`);

// Ключи, реально используемые в коде — t('key' / t("key" / data-i18n="key".
const usedKeys = new Set();
for (const m of mainSrc.matchAll(/\bt\(\s*['"]([\w.]+)['"]/g)) usedKeys.add(m[1]);
for (const m of htmlSrc.matchAll(/data-i18n(?:-\w+)?="([\w.]+)"/g)) usedKeys.add(m[1]);

assert.ok(usedKeys.size > 30, `подозрительно мало найденных ключей: ${usedKeys.size}`);

const missing = [...usedKeys].filter((k) => !ruKeys.has(k));
assert.deepStrictEqual(missing, [], `используемые ключи без перевода: ${missing.join(', ')}`);

// Плейсхолдеры `{name}` должны совпадать между ru и en для одного ключа —
// иначе interpolate() в одном языке молча оставит "{name}" в тексте.
function placeholders(str) {
  return [...str.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
}
const mismatched = [];
for (const key of ruKeys) {
  const a = placeholders(DICT.ru[key]);
  const b = placeholders(DICT.en[key]);
  if (JSON.stringify(a) !== JSON.stringify(b)) mismatched.push(`${key}: ru=${a} en=${b}`);
}
assert.deepStrictEqual(mismatched, [], `расходятся плейсхолдеры ru/en:\n${mismatched.join('\n')}`);

console.log(
  `i18n.check.js: OK — ${ruKeys.size} ключей в словаре, ${usedKeys.size} используются в коде, ru/en синхронны.`
);
