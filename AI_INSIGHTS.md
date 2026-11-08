# Zed + AI Insights (форк)

Форк Zed 1.27.0 stable (`c84af5b`) с тремя AI-функциями. Работают с любым провайдером, настроенным в
Zed: по умолчанию это подписка ChatGPT (`openai-subscribed`), но подойдут и DeepSeek, OpenAI, Anthropic,
Google, Mistral, xAI, OpenRouter, Ollama, LM Studio и любой OpenAI-совместимый сервер. API-ключи
вводятся в самом Zed: Agent Panel → Settings → LLM Providers.

- **Автокомплит.** Провайдер edit predictions `ai_insights`: серый текст на месте курсора, Tab принимает.
- **Описания.** Над каждой функцией и структурой данных появляется строка с кратким описанием.
  Описания кэшируются в `%LOCALAPPDATA%\Zed\ai_insights\descriptions.json`.
- **Советы.** Если модель узнаёт, что вы реализуете (LRU-кэш, Дейкстру, rate limiter…), появляется
  карточка с советами: над текущей функцией или над строкой курсора, но никогда под ней, чтобы не
  мешать серому тексту автокомплита. Пока подсказка автокомплита на экране, карточка ждёт.
  Esc или крестик её закрывают, закрытый совет не повторяется.

Код: `crates/ai_insights`, плюс вариант `AiInsights` в `EditPredictionProvider`.

## Выбор модели

Через меню иконки ✦ в статус-баре (когда провайдер автокомплита `ai_insights`) или через палитру
команд:

- `ai insights: select completion model` выбирает модель для автокомплита;
- `ai insights: select insights model` выбирает модель для описаний и советов.

Выбор сохраняется в `settings.json`. Например, DeepSeek для автокомплита и подписка для остального:

```jsonc
"ai_insights": {
  "provider": "openai-subscribed", "model": "gpt-6.1-terra",
  "completion_provider": "deepseek", "completion_model": "deepseek-chat"
}
```

## Настройки

```jsonc
"ai_insights": {
  "descriptions": true,
  "advice": true,
  "language": "ru",                  // язык описаний и советов
  "provider": "openai-subscribed",   // провайдер для описаний и советов
  "model": "gpt-6.1-terra",          // модель для описаний и советов
  "completion_provider": null,       // null: тот же провайдер
  "completion_model": null,          // null: та же модель (или быстрая модель другого провайдера)
  "effort": "medium",                // для описаний и советов
  "completion_effort": "low",        // для автокомплита; модели без reasoning его игнорируют
  "completion_debounce_ms": 500,
  "advice_delay_ms": 2000,
  "max_file_bytes": 80000
},
// Сборка работает на канале preview, поэтому провайдер включается только для неё:
"preview": {
  "edit_predictions": { "provider": "ai_insights" }
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
