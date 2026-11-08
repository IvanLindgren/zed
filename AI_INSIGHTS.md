# Zed + AI Insights (форк)

Форк Zed 1.27.0 stable (`c84af5b`) с тремя AI-функциями. Все они работают через встроенный в Zed
провайдер подписки ChatGPT (`openai-subscribed`), без отдельных API-ключей.

- **Автокомплит.** Провайдер edit predictions `chatgpt`: серый текст на месте курсора, Tab принимает.
- **Описания.** Над каждой функцией и структурой данных появляется строка с кратким описанием.
  Описания кэшируются в `%LOCALAPPDATA%\Zed\ai_insights\descriptions.json`.
- **Советы.** Если модель узнаёт, что вы реализуете (LRU-кэш, Дейкстру, rate limiter…), под строкой
  появляется карточка с советами. Esc или крестик её закрывают, закрытый совет не повторяется.

Код: `crates/ai_insights`, плюс вариант `ChatGpt` в `EditPredictionProvider`.

## Настройки

```jsonc
"ai_insights": {
  "descriptions": true,
  "advice": true,
  "language": "ru",            // язык описаний и советов
  "model": "gpt-6.1-terra",    // id модели из подписки
  "effort": "medium",          // для описаний и советов
  "completion_effort": "low",  // для автокомплита
  "completion_debounce_ms": 500,
  "advice_delay_ms": 2000,
  "max_file_bytes": 80000
},
// Сборка работает на канале preview, поэтому провайдер включается только для неё:
"preview": {
  "edit_predictions": { "provider": "chatgpt" }
}
```

## Сборка (Windows)

Нужны VS 2022 Build Tools с C++, Spectre-библиотеками и Windows SDK, а также CMake
(см. `docs/src/development/windows.md`).

```bat
script\ai-insights-build.cmd
```

Результат лежит в `target\release\zed.exe`. Сборка идёт на канале `preview`: она работает рядом с
обычным Zed, а автообновление в ней отключено.

- `script\ai-insights-cargo.cmd` запускает cargo в окружении VS 2022 Build Tools. Это нужно, если
  установлена более новая VS без Spectre-библиотек.
- Если загрузка WebRTC во время сборки обрывается по таймауту, скачайте
  `webrtc-win-x64-release.zip` (ссылка в логе ошибки) и распакуйте в `..\webrtc-prebuilt\`. Скрипт
  сборки подхватит его оттуда.
