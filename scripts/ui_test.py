"""Браузерный тест клиента Шторки (Chromium + Playwright).
Запуск: сервер на 127.0.0.1:7878 и статика клиента на :8000 (см. run_ui_tests.sh)."""
import base64, os, sys, time
from playwright.sync_api import sync_playwright, expect

CLIENT = "http://127.0.0.1:8000/index.html"
SHOTS = os.environ.get("SHOTS", "/tmp/shots")
os.makedirs(SHOTS, exist_ok=True)
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
os.makedirs("/tmp/shtorka-ui", exist_ok=True)
with open("/tmp/shtorka-ui/pixel.png", "wb") as f:
    f.write(PNG)

def check(cond, msg):
    print(("OK  " if cond else "FAIL"), msg)
    if not cond:
        raise SystemExit(1)

with sync_playwright() as p:
    browser = p.chromium.launch(
        executable_path="/usr/bin/chromium",
        args=["--no-sandbox", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", "--autoplay-policy=no-user-gesture-required"],
    )
    ctx = browser.new_context(viewport={"width": 1100, "height": 720}, permissions=["microphone", "camera"])
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    page.goto(CLIENT)
    page.evaluate("localStorage.clear()")
    page.reload()

    # Тема по умолчанию — тёмная; переключаем на светлую и обратно.
    check(page.evaluate("document.documentElement.dataset.theme") == "dark", "тема по умолчанию тёмная")
    page.click("#login-theme")  # dark -> light
    check(page.evaluate("document.documentElement.dataset.theme") == "light", "переключение на светлую")
    page.screenshot(path=f"{SHOTS}/login-light.png")
    page.click("#login-theme")  # light -> auto
    page.click("#login-theme")  # auto -> dark
    check(page.evaluate("document.documentElement.dataset.theme") == "dark", "цикл тем возвращается к тёмной")

    # Вход.
    page.fill("#server", "127.0.0.1:7878")
    page.fill("#name", "Вася")
    page.click("#connect")
    expect(page.locator("#status")).to_have_text("в сети", timeout=8000)
    check(page.is_visible("#app"), "вход в приложение")

    # Текст.
    page.fill("#input", "привет, это тест")
    page.press("#input", "Enter")
    expect(page.locator(".bubble .text", has_text="привет, это тест")).to_be_visible(timeout=5000)
    check(page.locator(".row.out").count() >= 1, "своё сообщение справа (out)")
    check(page.locator("#input").input_value() == "", "поле очищается после отправки")

    # Фото через file input.
    page.set_input_files("#file-input", "/tmp/shtorka-ui/pixel.png")
    expect(page.locator(".bubble img.media-img")).to_be_visible(timeout=8000)
    check(True, "фото отображается в пузыре")

    # Файл-документ.
    with open("/tmp/shtorka-ui/notes.txt", "w") as f:
        f.write("тестовый файл\n")
    page.set_input_files("#file-input", "/tmp/shtorka-ui/notes.txt")
    expect(page.locator("a.file", has_text="notes.txt")).to_be_visible(timeout=8000)
    check(True, "документ отображается ссылкой")

    # Голосовое.
    page.click("#voice")
    expect(page.locator("#rec-bar")).to_be_visible(timeout=5000)
    time.sleep(1.5)
    page.screenshot(path=f"{SHOTS}/recording-voice.png")
    page.click("#rec-send")
    expect(page.locator(".bubble .audio audio")).to_be_visible(timeout=10000)
    check(True, "голосовое отправлено и воспроизводится в пузыре")

    # Видео-кружок.
    page.click("#note")
    expect(page.locator("#note-preview")).to_be_visible(timeout=5000)
    time.sleep(1.5)
    page.screenshot(path=f"{SHOTS}/recording-video.png")
    page.click("#rec-send")
    expect(page.locator("video.note")).to_be_visible(timeout=10000)
    check(True, "видео-кружок отправлен (круглое видео)")

    # Отмена записи не отправляет ничего.
    before = page.locator(".bubble").count()
    page.click("#voice")
    time.sleep(0.8)
    page.click("#rec-cancel")
    time.sleep(1.0)
    check(page.locator(".bubble").count() == before, "отмена записи ничего не отправляет")

    # Светлая тема со всеми пузырями.
    page.click("#theme-btn")
    page.click("#theme-btn")
    time.sleep(0.6)
    check(page.evaluate("document.documentElement.dataset.theme") == "light", "светлая тема в приложении")
    page.screenshot(path=f"{SHOTS}/chat-light.png", full_page=False)
    page.click("#theme-btn")
    time.sleep(0.6)
    page.screenshot(path=f"{SHOTS}/chat-dark.png", full_page=False)

    # Ошибка: занятое имя (второе окно с тем же именем, пока первое в сети).
    ctx2 = browser.new_context(viewport={"width": 900, "height": 600})
    p2 = ctx2.new_page()
    p2.goto(CLIENT)
    p2.evaluate("localStorage.clear()")
    p2.reload()
    p2.fill("#server", "127.0.0.1:7878")
    p2.fill("#name", "Вася")
    p2.click("#connect")
    time.sleep(2)
    check("занято" in p2.locator("#login-error").inner_text() and p2.is_visible("#login"), "вторая вкладка с тем же именем получает «Имя уже занято» на входе")
    ctx2.close()

    browser.close()
    real_errors = [e for e in errors if "favicon" not in e and "Failed to load resource" not in e]
    print("JS-ошибки:", real_errors if real_errors else "нет")
    print("ВСЕ ПРОВЕРКИ ПРОЙДЕНЫ")
