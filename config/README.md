# localhost-rs — HTTP/1.1 сервер на Rust (один поток, epoll)

Единственная внешняя зависимость — `libc`. Никаких tokio/nix.

## Сборка и запуск

```bash
cargo build --release
./target/release/localhost config/server.conf   # запускать из корня проекта
```

Остановка: `Ctrl+C` / `SIGTERM` (корректное завершение, удобно для valgrind).

## Тесты

```bash
cargo test                          # юнит-тесты (парсер, конфиг, роутинг, коды статусов, CGI, сессии)
python3 tests/integration.py        # сквозные тесты (сам поднимает сервер)
python3 tests/integration.py --slow # + проверка таймаутов (408, 504), ~30 с
scripts/siege.sh                    # siege -b, цель: доступность >= 99.5%
scripts/leakcheck.sh                # valgrind + интеграционные тесты
scripts/compare_nginx.sh /files/    # сравнить заголовки с NGINX
```

## Структура

| Файл | Назначение |
|---|---|
| `src/epoll.rs` | обёртка над epoll, `O_NONBLOCK`, `RLIMIT_NOFILE` |
| `src/server.rs` | listeners, цикл событий, состояния соединений, таймауты, интеграция CGI |
| `src/http.rs` | инкрементальный парсер (Content-Length и chunked), сборка ответа, date/mime/url-утилиты |
| `src/config.rs` | парсер конфига и валидация |
| `src/router.rs` | выбор виртуального хоста, longest-prefix маршруты, защита от `..` |
| `src/handler.rs` | статика, autoindex, upload (multipart/raw), DELETE, редиректы, CGI-диспетчер, сессии |
| `src/cgi.rs` | запуск интерпретатора с неблокирующими пайпами, разбор вывода CGI |
| `src/session.rs` | cookie-сессии (id из `/dev/urandom`, TTL, лимит) |

## Формат конфига

```
server {
    host 127.0.0.1                 # server_address
    ports 8080 8081                # один или несколько портов
    server_name localhost a.local  # первый server для host:port — по умолчанию
    client_max_body_size 1M        # k/M/G; допустимо 1 .. 1G
    error_page 404 www/errors/404.html

    route /path {
        methods GET POST DELETE    # GET HEAD POST DELETE
        root www                   # каталог или файл
        index index.html           # файл по умолчанию для каталога
        autoindex on|off           # листинг каталога
        cgi .py /usr/bin/python3   # CGI по расширению
        redirect 301 /elsewhere    # редирект (301/302/303/307/308)
        upload_dir www/uploads     # куда класть загрузки (POST)
    }
}
```

Маршрут `/test` с `root /usr/Desktop` отдаёт `/test/my_page.html` из `/usr/Desktop/my_page.html`.
Регулярные выражения не поддерживаются; строки с `#` игнорируются.

Несколько `server` с одним `host:port` — виртуальные хосты (выбор по `Host`), у них
должны различаться `server_name`, иначе валидация выдаст ошибку «conflict».

## Как выполнены требования задания

- **Один процесс, один поток**, все сокеты и пайпы CGI — `O_NONBLOCK`.
- **epoll**: один `epoll_wait` на итерацию цикла; любые `read`/`write` — только после события.
  Ответ после разбора запроса не пишется сразу: соединение переключается на `EPOLLOUT`.
- **Несколько портов и серверов**: серверы с общим `host:port` делят один listening-сокет.
- **Таймауты**: 15 с без байтов / 30 с на весь запрос (→ 408), keep-alive idle 15 с,
  запись 30 с, CGI 10 с (→ 504, процесс убивается и забирается через `wait`).
- **Не падать**: нет `unwrap` на пользовательских данных, ошибка соединения = закрытие соединения,
  обработчик каждого события обёрнут в `catch_unwind`, лимит соединений по `RLIMIT_NOFILE`.
- **Методы**: GET, HEAD, POST, DELETE; остальные → 405 с заголовком `Allow`.
- **Upload**: `multipart/form-data` и «сырое» тело (`POST /upload?name=f.bin`); имена файлов
  очищаются, коллизии разрешаются суффиксом `_N`. Лимит проверяется по мере чтения → 413.
- **Chunked/unchunked** запросы; тело CGI всегда передаётся уже декодированным.
- **Cookies/сессии**: встроенный `/__session[?name=X]` (выдаёт `SID`, считает визиты) и `/__session/logout`.
  Заголовок `Cookie` также доступен CGI как `HTTP_COOKIE`.
- **Страницы ошибок** по умолчанию для любых кодов + свои через `error_page` (400/403/404/405/413/500 есть в `www/errors`).
- **CGI**: Python; `fork+exec` через `std::process::Command`, скрипт — первый аргумент, `chdir` в каталог скрипта,
  `PATH_INFO` = полный путь скрипта, EOF на stdin = конец тела.
- **Безопасность путей**: `..` выше root → 403; симлинки наружу root → 403.

## Известные ограничения

- Статические файлы читаются в память целиком (до 256 МБ) обычным `fs::read`: обычные файлы
  не поддерживаются epoll, дисковый ввод-вывод синхронный.
- `Expect: 100-continue` не обрабатывается (curl подождёт ~1 с и всё равно отправит тело).
- Нет TLS, Range-запросов и сжатия.
