// resticker — локализация окна настроек (ROADMAP.md M8, «локализация:
// русский и английский»). Без фреймворка/бандлера — тот же принцип, что
// main.js: обычный скрипт, словарь + два маленьких хелпера.
//
// Область: ТОЛЬКО это Tauri-окно (settings). Нативный оверлей (тулбар,
// панель выбора окон, панель пресетов у курсора) рисует текст растровым
// шрифтом с точными пиксельными тестами ширины — там локализация отдельный,
// более рискованный срез, не сделан здесь намеренно (см. ROADMAP.md).
//
// Ключи с параметрами используют `{name}`-плейсхолдеры, см. `t()`.

const DICT = {
  ru: {
    'window.title': 'resticker — настройки',
    'window.header': 'resticker · настройки',
    'close': 'Закрыть',

    'tab.stickers': 'Стикеры',
    'tab.presets': 'Пресеты',
    'tab.control': 'Управление',
    'tab.general': 'Общие',
    'tab.denylist': 'Денй-лист',

    'stickers.listLabel': 'Список стикеров',
    'stickers.add': 'Добавить…',
    'stickers.loading': 'Загрузка…',
    'stickers.resetAll': 'Сбросить всё',
    'stickers.deleteAll': 'Удалить всё',
    'stickers.empty': 'Стикеров пока нет.',
    'stickers.resetAllDone': 'Все стикеры сброшены',
    'stickers.deleteAllConfirm': 'Удалить все стикеры? Это действие нельзя отменить.',
    'stickers.deleteAllDone': 'Все стикеры удалены',
    'stickers.added': 'Добавлено: {path}',
    'stickers.dialogFilter': 'Изображения и видео',
    'sticker.toggleTitle': 'Включить/выключить',
    'sticker.resetPosTitle': 'Сбросить позицию',
    'sticker.resetTransformTitle': 'Сбросить размер и поворот',
    'sticker.revealTitle': 'Показать в проводнике',
    'sticker.relinkTitle': 'Переуказать файл',
    'sticker.deleteTitle': 'Удалить',
    'sticker.monitorLabel': 'монитор {id}',
    'media.window': 'Окно',
    'media.video': 'Видео',
    'media.animation': 'Анимация',
    'media.image': 'Изображение',

    'presets.listLabel': 'Список пресетов',
    'presets.import': 'Импорт из файла…',
    'presets.importTitle': 'Импортировать пресет из файла .json',
    'presets.namePlaceholder': 'Имя нового пресета',
    'presets.save': 'Сохранить как пресет',
    'presets.rename': 'Переименовать',
    'presets.loading': 'Загрузка…',
    'presets.empty': 'Пресетов пока нет. Сохраните текущую расстановку выше.',
    'presets.dialogFilter': 'Пресет resticker',
    'preset.applyAction': 'Применить',
    'preset.applyTitle': 'Заменить текущую расстановку',
    'preset.renameTitle': 'Переименовать',
    'preset.exportAction': 'Экспорт',
    'preset.exportTitle': 'Экспорт в файл .json',
    'preset.deleteTitle': 'Удалить',
    'preset.applied': 'Пресет «{name}» применён',
    'preset.renamingStatus': 'Переименование пресета «{name}» — нажмите «Переименовать»',
    'preset.exportConfirm':
      'Пресет содержит абсолютные пути к файлам на вашем диске и имена программ. Экспортировать?',
    'preset.exported': 'Пресет экспортирован: {path}',
    'preset.deleteConfirm': 'Удалить пресет «{name}»?',
    'preset.deleted': 'Пресет удалён',
    'preset.saved': 'Пресет «{name}» сохранён',
    'preset.renamed': 'Пресет переименован',
    'preset.imported': 'Пресет импортирован: {path}',
    'preset.stickersCount': '{n} {word}',
    'preset.word.one': 'стикер',
    'preset.word.few': 'стикера',
    'preset.word.many': 'стикеров',
    'preset.missingAlert':
      'Пресет применён не полностью — недоступны:\n\n{lines}\n\nЗагружены остальные стикеры.',

    'denylist.listLabel': 'Денй-лист закрепления',
    'denylist.hint':
      'Окна, чей процесс или заголовок совпадает с правилом, нельзя закрепить горячей клавишей — они не показываются в списке выбора окна. Больше денй-лист ни на что не влияет.',
    'denylist.pickWindow': 'Выбрать открытое окно…',
    'denylist.processPlaceholder': 'process_name.exe',
    'denylist.titlePlaceholder': 'часть заголовка, * — подстановка',
    'denylist.add': 'Добавить',
    'denylist.loading': 'Загрузка…',
    'denylist.empty': 'Денй-лист пуст. Добавьте процесс выше, чтобы запретить его закрепление.',
    'denylist.noTitlePattern': 'без фильтра по заголовку',
    'denylist.removeTitle': 'Удалить',
    'denylist.added': 'Добавлено в денй-лист: {process}',
    'denylist.removed': 'Правило удалено из денй-листа',

    'control.hotkeysLabel': 'Глобальные хоткеи:',
    'control.editMode': 'Режим редактирования',
    'control.toggleAll': 'Показать/скрыть все стикеры',
    'control.muteAll': 'Заглушить всё',
    'control.pinFocusedWindow': 'Закрепить активное окно',
    'control.pinSoundVolume': 'Громкость закрепления окна',
    'general.outlinePinned': 'Обводка на закреплённом окне',
    'general.outlinePinnedHint':
      'Пока окно закреплено, вокруг него держится рамка. Без опции закрепление видно только по булавке в углу окна.',
    'control.notSet': 'Не задан',
    'control.recording': 'Нажмите комбинацию…',
    'control.hintClick': 'Клик по полю, затем нажмите комбинацию клавиш.',
    'control.hintRestart': 'Изменения хоткеев применяются после перезапуска resticker.',
    'control.hintRequired':
      '«Режим редактирования» — обязательная комбинация (нельзя очистить). Остальные — опциональны.',

    'general.startupLabel': 'Запуск:',
    'general.autostart': 'Автозапуск с Windows',
    'general.silentStart': 'Тихий старт (не открывать настройки)',
    'general.trayIcon': 'Иконка в трее',
    'general.dialogsLabel': 'Диалоги:',
    'general.skipDeleteConfirm': 'Не спрашивать подтверждение при удалении',
    'general.screenLabel': 'Экран:',
    'general.hideFromCapture': 'Скрывать стикеры от захвата экрана',
    'general.hideFromCaptureHint':
      'На отдельных сборках Windows 11 работает нестабильно — результат применения проверяется, при неудаче будет предупреждение в логе.',
    'general.neverOverlapTaskbar': 'Не перекрывать панель задач',
    'general.performanceLabel': 'Производительность:',
    'general.fpsLimit': 'Лимит FPS оверлея',
    'general.languageLabel': 'Язык',

    'footer.apply': 'Применить',
    'footer.applied': 'Применено',
    'footer.ok': 'ОК',
    'footer.cancel': 'Отмена',

    'error.generic': 'Ошибка: {err}',
    'error.configLoad': 'Ошибка загрузки config.json: {err}',
  },
  en: {
    'window.title': 'resticker — settings',
    'window.header': 'resticker · settings',
    'close': 'Close',

    'tab.stickers': 'Stickers',
    'tab.presets': 'Presets',
    'tab.control': 'Controls',
    'tab.general': 'General',
    'tab.denylist': 'Denylist',

    'stickers.listLabel': 'Sticker list',
    'stickers.add': 'Add…',
    'stickers.loading': 'Loading…',
    'stickers.resetAll': 'Reset all',
    'stickers.deleteAll': 'Delete all',
    'stickers.empty': 'No stickers yet.',
    'stickers.resetAllDone': 'All stickers reset',
    'stickers.deleteAllConfirm': 'Delete all stickers? This cannot be undone.',
    'stickers.deleteAllDone': 'All stickers deleted',
    'stickers.added': 'Added: {path}',
    'stickers.dialogFilter': 'Images and video',
    'sticker.toggleTitle': 'Enable/disable',
    'sticker.resetPosTitle': 'Reset position',
    'sticker.resetTransformTitle': 'Reset size and rotation',
    'sticker.revealTitle': 'Show in Explorer',
    'sticker.relinkTitle': 'Relink file',
    'sticker.deleteTitle': 'Delete',
    'sticker.monitorLabel': 'monitor {id}',
    'media.window': 'Window',
    'media.video': 'Video',
    'media.animation': 'Animation',
    'media.image': 'Image',

    'presets.listLabel': 'Preset list',
    'presets.import': 'Import from file…',
    'presets.importTitle': 'Import a preset from a .json file',
    'presets.namePlaceholder': 'New preset name',
    'presets.save': 'Save as preset',
    'presets.rename': 'Rename',
    'presets.loading': 'Loading…',
    'presets.empty': 'No presets yet. Save your current layout above.',
    'presets.dialogFilter': 'resticker preset',
    'preset.applyAction': 'Apply',
    'preset.applyTitle': 'Replace current layout',
    'preset.renameTitle': 'Rename',
    'preset.exportAction': 'Export',
    'preset.exportTitle': 'Export to .json file',
    'preset.deleteTitle': 'Delete',
    'preset.applied': 'Preset "{name}" applied',
    'preset.renamingStatus': 'Renaming preset "{name}" — press "Rename" to confirm',
    'preset.exportConfirm':
      'This preset contains absolute file paths on your disk and program names. Export anyway?',
    'preset.exported': 'Preset exported: {path}',
    'preset.deleteConfirm': 'Delete preset "{name}"?',
    'preset.deleted': 'Preset deleted',
    'preset.saved': 'Preset "{name}" saved',
    'preset.renamed': 'Preset renamed',
    'preset.imported': 'Preset imported: {path}',
    'preset.stickersCount': '{n} {word}',
    'preset.word.one': 'sticker',
    'preset.word.few': 'stickers',
    'preset.word.many': 'stickers',
    'preset.missingAlert':
      'Preset applied partially — unavailable:\n\n{lines}\n\nThe rest of the stickers were loaded.',

    'denylist.listLabel': 'Pin denylist',
    'denylist.hint':
      'Windows whose process or title matches a rule cannot be pinned via the hotkey and are hidden from the window pick list. The denylist has no other effect.',
    'denylist.pickWindow': 'Pick an open window…',
    'denylist.processPlaceholder': 'process_name.exe',
    'denylist.titlePlaceholder': 'title substring, * wildcard',
    'denylist.add': 'Add',
    'denylist.loading': 'Loading…',
    'denylist.empty': 'The denylist is empty. Add a process above to prevent it from being pinned.',
    'denylist.noTitlePattern': 'no title filter',
    'denylist.removeTitle': 'Remove',
    'denylist.added': 'Added to denylist: {process}',
    'denylist.removed': 'Rule removed from the denylist',

    'control.hotkeysLabel': 'Global hotkeys:',
    'control.editMode': 'Edit mode',
    'control.toggleAll': 'Show/hide all stickers',
    'control.muteAll': 'Mute all',
    'control.pinFocusedWindow': 'Pin focused window',
    'control.pinSoundVolume': 'Pin sound volume',
    'general.outlinePinned': 'Outline on pinned window',
    'general.outlinePinnedHint':
      'Keeps an outline around a window while it stays pinned. Without it, the only permanent sign is the pin badge in the corner.',
    'control.notSet': 'Not set',
    'control.recording': 'Press a combination…',
    'control.hintClick': 'Click a field, then press a key combination.',
    'control.hintRestart': 'Hotkey changes apply after restarting resticker.',
    'control.hintRequired':
      '"Edit mode" is required (cannot be cleared). The others are optional.',

    'general.startupLabel': 'Startup:',
    'general.autostart': 'Launch with Windows',
    'general.silentStart': 'Silent start (don’t open settings)',
    'general.trayIcon': 'Tray icon',
    'general.dialogsLabel': 'Dialogs:',
    'general.skipDeleteConfirm': 'Skip delete confirmation',
    'general.screenLabel': 'Screen:',
    'general.hideFromCapture': 'Hide stickers from screen capture',
    'general.hideFromCaptureHint':
      'Unstable on some Windows 11 builds — the result is verified after applying; a warning is logged on failure.',
    'general.neverOverlapTaskbar': "Don't overlap the taskbar",
    'general.performanceLabel': 'Performance:',
    'general.fpsLimit': 'Overlay FPS limit',
    'general.languageLabel': 'Language',

    'footer.apply': 'Apply',
    'footer.applied': 'Applied',
    'footer.ok': 'OK',
    'footer.cancel': 'Cancel',

    'error.generic': 'Error: {err}',
    'error.configLoad': 'Failed to load config.json: {err}',
  },
};

/** Текущий язык интерфейса — по умолчанию русский, пока конфиг не загружен. */
let currentLang = 'ru';

/** Подставить `{name}` в строку из `vars[name]` (без вложенности/логики —
 *  ровно то, что нужно этим сообщениям). */
function interpolate(str, vars) {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (m, key) => (key in vars ? String(vars[key]) : m));
}

/** Перевести ключ на текущий язык; отсутствующий ключ — сам ключ (заметно
 *  в UI, не проваливается в пустоту молча). */
function t(key, vars) {
  const table = DICT[currentLang] ?? DICT.ru;
  const str = table[key] ?? DICT.ru[key] ?? key;
  return interpolate(str, vars);
}

/** Число стикеров пресета с русским множественным числом (1/2-4/5+) — на
 *  английском число не влияет на форму слова, но ключ общий. */
function pluralStickerWord(n) {
  if (currentLang !== 'ru') return t('preset.word.many');
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (mod10 === 1 && mod100 !== 11) return t('preset.word.one');
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return t('preset.word.few');
  return t('preset.word.many');
}

/** Применить перевод ко всем статическим узлам разметки, помеченным
 *  `data-i18n`/`data-i18n-placeholder`/`data-i18n-title`, и переключить язык
 *  документа — вызывается при загрузке и сразу при смене языка в «Общие»
 *  (без перезапуска окна). */
function applyStaticTranslations(lang) {
  currentLang = DICT[lang] ? lang : 'ru';
  document.documentElement.lang = currentLang;
  document.title = t('window.title');
  for (const el of document.querySelectorAll('[data-i18n]')) {
    el.textContent = t(el.getAttribute('data-i18n'));
  }
  for (const el of document.querySelectorAll('[data-i18n-placeholder]')) {
    el.placeholder = t(el.getAttribute('data-i18n-placeholder'));
  }
  for (const el of document.querySelectorAll('[data-i18n-title]')) {
    el.title = t(el.getAttribute('data-i18n-title'));
  }
  for (const el of document.querySelectorAll('[data-i18n-alt]')) {
    el.alt = t(el.getAttribute('data-i18n-alt'));
  }
}
