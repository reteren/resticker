// resticker — тексты окна настроек. Без фреймворка/бандлера — тот же
// принцип, что main.js: обычный скрипт, таблица строк + два хелпера.
//
// Язык один — английский (запрос пользователя 2026-08-23: «переведи всю
// программу на английский»). Раньше здесь жила пара словарей ru/en и
// переключатель языка в «Общих», но нативный оверлей не переводился вовсе
// и оставался русским — при любом выборе программа выходила смешанной.
// Таблица ключей осталась на месте: вернуть второй язык — это вернуть
// `DICT` с двумя ветками и селектор (история git до 2026-08-23), а не
// переписывать разметку.
//
// Ключи с параметрами используют `{name}`-плейсхолдеры, см. `t()`.

const STRINGS = {
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
  'stickers.empty': 'No stickers yet. Add an image or a video and it will appear on your desktop.',
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
  'denylist.anyProcess': 'any process, title match only',
  'denylist.removeTitle': 'Remove',
  'denylist.added': 'Added to denylist: {process}',
  'denylist.removed': 'Rule removed from the denylist',

  'control.hotkeysLabel': 'Global hotkeys',
  'control.editMode': 'Edit mode',
  'control.toggleAll': 'Show/hide all stickers',
  'control.muteAll': 'Mute all',
  'control.unpinAll': 'Unpin all windows',
  'control.groupsMenu': 'Window groups menu',
  'control.openGroup': 'Open group',
  'control.openGroupNumber': '(number of group)',
  'control.openGroupHint':
    'Only the modifiers are set here — the digit is the group number (1–9), fixed for each group and not editable. Press the combination with any digit.',
  'control.deleteGroup': 'Delete open group',
  'control.pinGroup': 'Keep open group on top',
  'control.pinFocusedWindow': 'Pin focused window',
  'control.mitosis': 'Split a window in two (mitosis)',
  'control.pinSoundVolume': 'Pin sound volume',
  'general.snapGap': 'Snap gap, %',
  'general.snapGapHint':
    'Shrinks a window snapped by Windows to a half, quarter or third of the screen by this much on every side, leaving an even gap around it. 0 turns it off. Also available in the tray menu, where you can extend it to unpinned windows.',
  'general.outlinePinned': 'Outline on pinned window',
  'general.outlinePinnedHint':
    'Keeps an outline around a window while it stays pinned. Without it, the only permanent sign is the pin badge in the corner.',
  'control.notSet': 'Not set',
  'control.clear': 'Clear',
  'control.groupsSection': 'Groups',
  'control.soundSection': 'Sound',
  'control.recording': 'Press a combination…',
  'control.modifierRequired': 'At least one modifier is required (Ctrl, Alt, Shift, or Win).',
  'control.unsupportedKey': 'Key "{key}" is not supported. Use A-Z, 0-9, or F1-F24.',
  'control.hintClick': 'Click a field, then press a combination. Esc cancels.',
  'control.hintRestart': 'Hotkey changes apply immediately after saving.',
  'control.hintRequired':
    '"Edit mode" is required (cannot be cleared). The others are optional.',

  'general.startupLabel': 'Startup',
  'general.autostart': 'Launch with Windows',
  'general.silentStart': 'Silent start (don’t open settings)',
  'general.trayIcon': 'Tray icon',
  'general.dialogsLabel': 'Dialogs',
  'general.skipDeleteConfirm': 'Skip delete confirmation',
  'general.screenLabel': 'Screen',
  'general.hideFromCapture': 'Hide stickers from screen capture',
  'general.hideFromCaptureHint':
    'Unstable on some Windows 11 builds — the result is verified after applying; a warning is logged on failure.',
  'general.neverOverlapTaskbar': "Don't overlap the taskbar",
  'general.performanceLabel': 'Performance',
  'general.fpsLimit': 'Overlay FPS limit',
  'general.mitosisMemoryLimit': 'Mitosis memory limit, MB',
  'general.mitosisMemoryLimitHint':
    'A heavier app is refused: mitosis launches a second copy of it.',

  'footer.apply': 'Apply',
  'footer.applied': 'Applied',
  'footer.ok': 'OK',
  'footer.cancel': 'Cancel',

  'error.generic': 'Error: {err}',
  'error.configLoad': 'Failed to load config.json: {err}',
};

/** Подставить `{name}` в строку из `vars[name]` (без вложенности/логики —
 *  ровно то, что нужно этим сообщениям). */
function interpolate(str, vars) {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (m, key) => (key in vars ? String(vars[key]) : m));
}

/** Текст по ключу; отсутствующий ключ — сам ключ (заметно в UI, не
 *  проваливается в пустоту молча). */
function t(key, vars) {
  return interpolate(STRINGS[key] ?? key, vars);
}

/** Слово «стикер(ы)» в подписи пресета. Раньше английская ветка всегда
 *  возвращала множественное число и писала «1 stickers» — теперь, когда
 *  язык один, форма выбирается по числу. */
function pluralStickerWord(n) {
  return t(n === 1 ? 'preset.word.one' : 'preset.word.many');
}

/** Проставить тексты всем статическим узлам разметки, помеченным
 *  `data-i18n`/`data-i18n-placeholder`/`data-i18n-title`/`data-i18n-alt` —
 *  вызывается один раз при загрузке окна. */
function applyStaticTranslations() {
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
