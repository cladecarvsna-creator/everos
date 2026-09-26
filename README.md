# EverOS

EverOS — операционная система для x86_64 на ASM и Rust.

Загрузчик GRUB (multiboot2) передаёт управление коду на ассемблере, тот
проверяет процессор, включает страничную память и 64-битный режим (long mode)
и вызывает ядро на Rust.

## Что умеет

- **Графика**: GRUB включает режим 1024x768x32, ядро рисует по framebuffer
  (пиксели, прямоугольники, линии, круги, градиенты). Команда `gfx` показывает демо.
  Если GRUB не дал графику, ядро работает в текстовом режиме VGA 80x25.
- **Текст**: `print!`/`println!` с цветами, перенос строк, прокрутка, мигающий
  курсор. Шрифт Terminus 8x16: латиница, кириллица, рамки. Весь вывод дублируется
  в последовательный порт.
- **Прерывания**: IDT, заглушки на ассемблере (`boot/interrupts.asm`), PIC 8259,
  таймер PIT на 100 Гц. Исключения процессора выводятся как kernel panic.
- **Клавиатура PS/2**: раскладки EN и RU (переключение Alt+Shift), Shift, Caps Lock, Ctrl.
- **Мышь PS/2**: указатель мыши поверх текста и графики, координаты в строке состояния.
- **Оболочка**: команды `help`, `clear`, `echo`, `info`, `colors`, `gfx`, `panic`.
  Стрелка вверх повторяет прошлую команду, Ctrl+L очищает экран, Ctrl+C сбрасывает строку.

## Структура

| Путь | Что там |
| --- | --- |
| `boot/multiboot_header.asm` | заголовок multiboot2 для GRUB |
| `boot/boot.asm` | 32-битный старт: проверки CPU, таблицы страниц, переход в long mode |
| `boot/long_mode.asm` | 64-битная точка входа, вызывает `kernel_main` |
| `boot/interrupts.asm` | точки входа прерываний, вызывают `interrupt_dispatch` в Rust |
| `kernel/` | ядро на Rust (`no_std`, цель `x86_64-unknown-none`) |
| `kernel/src/console.rs` | консоль: текст, цвета, прокрутка, курсор, указатель мыши |
| `kernel/src/framebuffer.rs` | рисование по framebuffer |
| `kernel/src/interrupts.rs` | IDT, PIC, таймер |
| `kernel/src/keyboard.rs`, `ps2.rs` | клавиатура и мышь PS/2 |
| `kernel/src/shell.rs` | оболочка |
| `fonts/`, `scripts/gen-font.py` | шрифт Terminus и генератор `kernel/src/font_data.rs` |
| `linker.ld` | скрипт линкера, ядро грузится по адресу 1 МиБ |
| `iso/boot/grub/grub.cfg` | конфиг GRUB для загрузочного ISO |
| `scripts/boot-test.sh` | запуск в QEMU без экрана: проверка старта ядра и ввода с клавиатуры |

## Запуск на Windows (без сборки и без WSL)

CI собирает ISO при каждом изменении в `main` и выкладывает его в релизы.

1. Установите QEMU: [qemu.weilnetz.de/w64](https://qemu.weilnetz.de/w64/)
   (установщик по умолчанию ставит в `C:\Program Files\qemu`) или командой
   `winget install SoftwareFreedomConservancy.QEMU`.
2. Скачайте свежий ISO:
   [everos.iso](https://github.com/cladecarvsna-creator/everos/releases/latest/download/everos.iso)
   (все сборки: [Releases](https://github.com/cladecarvsna-creator/everos/releases)).
3. Запустите в PowerShell из папки с ISO:

   ```powershell
   & "C:\Program Files\qemu\qemu-system-x86_64.exe" -cdrom everos.iso -m 256M
   ```

   Или положите рядом с ISO [`run-windows.bat`](scripts/run-windows.bat)
   (он есть и в релизе) и запустите его двойным щелчком.

В окне QEMU щёлкните мышью, чтобы QEMU захватил указатель (Ctrl+Alt+G отпускает).
Если QEMU не находит `-cdrom`, проверьте, что команда запущена в папке с `everos.iso`.

## Сборка из исходников

### Что нужно установить

- Rust (stable) через [rustup](https://rustup.rs); нужная цель поставится сама из `rust-toolchain.toml`
- `nasm`, `ld` (binutils), `make`
- `grub-mkrescue` (пакеты `grub-pc-bin`, `grub-common`), `xorriso`, `mtools`
- `qemu-system-x86_64`

На Ubuntu/Debian:

```sh
sudo apt install nasm build-essential grub-pc-bin grub-common xorriso mtools qemu-system-x86
```

### Команды

```sh
make        # собрать build/everos.iso
make run    # запустить в окне QEMU
make test   # загрузить без экрана, проверить старт ядра и ввод с клавиатуры
make clean  # удалить сборку
```

В окне QEMU щёлкните мышью, чтобы QEMU захватил указатель (Ctrl+Alt+G отпускает).

## Лицензия

MIT, см. [LICENSE](LICENSE). Шрифт Terminus Font (c) Dimitar Toshkov Zhekov,
SIL Open Font License 1.1, см. [fonts/LICENSE.terminus](fonts/LICENSE.terminus).
