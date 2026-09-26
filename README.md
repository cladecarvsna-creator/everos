# EverOS

EverOS — операционная система для x86_64 на ASM и Rust.

Загрузчик GRUB (multiboot2) передаёт управление коду на ассемблере, тот
проверяет процессор, включает страничную память и 64-битный режим (long mode)
и вызывает ядро на Rust.

## Что умеет

- **Рабочий стол**: система сразу загружается в графический рабочий стол с фоном,
  значками (двойной щелчок открывает программу), панелью задач с кнопкой Start,
  кнопками окон, раскладкой и часами. Окна можно перетаскивать за заголовок,
  сворачивать и закрывать, щелчок поднимает окно наверх. Всё рисуется во
  внеэкранный буфер и копируется на экран только изменившейся областью, поэтому
  ничего не мерцает. В меню Start есть перезагрузка и выключение.
- **Программы**:
  - *Terminal* — оболочка в окне;
  - *Paint* — рисование мышью: 24 цвета, кисть, ластик, заливка, 4 толщины,
    очистка; правая кнопка рисует белым;
  - *Calculator* — кнопки мышью или ввод с клавиатуры (цифры, `+ - * /`, `%`,
    Enter, Backspace, Esc);
  - *Graphics* — демо графики.
- **Графика**: GRUB включает режим 1024x768x32. Если GRUB не дал графику,
  ядро работает в текстовом режиме VGA 80x25 с одной оболочкой.
- **Текст**: `print!`/`println!` с цветами, перенос строк, прокрутка, мигающий
  курсор. Шрифт Terminus 8x16: латиница, кириллица, рамки. Весь вывод дублируется
  в последовательный порт.
- **Прерывания**: IDT, заглушки на ассемблере (`boot/interrupts.asm`), PIC 8259,
  таймер PIT на 100 Гц. Исключения процессора выводятся как kernel panic.
- **Клавиатура PS/2**: раскладки EN и RU (переключение Alt+Shift), Shift, Caps Lock, Ctrl.
- **Мышь PS/2**: указатель, щелчки левой и правой кнопкой, перетаскивание.
- **Оболочка**: команды `help`, `clear`, `echo`, `info`, `colors`, `paint`, `calc`,
  `gfx`, `exit` (закрыть окно терминала), `panic`.
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
| `kernel/src/framebuffer.rs` | рисование по framebuffer (загрузка и kernel panic) |
| `kernel/src/gui/` | рабочий стол: окна, панель задач, меню, `canvas.rs` (рисование в буфере), программы `terminal.rs`, `paint.rs`, `calc.rs`, `demo.rs` |
| `kernel/src/rtc.rs` | часы реального времени (CMOS) |
| `kernel/src/interrupts.rs` | IDT, PIC, таймер |
| `kernel/src/keyboard.rs`, `ps2.rs` | клавиатура и мышь PS/2 |
| `kernel/src/shell.rs` | оболочка |
| `fonts/`, `scripts/gen-font.py` | шрифт Terminus и генератор `kernel/src/font_data.rs` |
| `linker.ld` | скрипт линкера, ядро грузится по адресу 1 МиБ |
| `iso/boot/grub/grub.cfg` | конфиг GRUB для загрузочного ISO |
| `scripts/boot-test.sh` | запуск в QEMU без экрана: проверка старта ядра, рабочего стола и ввода с клавиатуры |

## Что нужно установить

- Rust (stable) через [rustup](https://rustup.rs); нужная цель поставится сама из `rust-toolchain.toml`
- `nasm`, `ld` (binutils), `make`
- `grub-mkrescue` (пакеты `grub-pc-bin`, `grub-common`), `xorriso`, `mtools`
- `qemu-system-x86_64`

На Ubuntu/Debian:

```sh
sudo apt install nasm build-essential grub-pc-bin grub-common xorriso mtools qemu-system-x86
```

## Сборка и запуск

```sh
make        # собрать build/everos.iso
make run    # запустить в окне QEMU
make test   # загрузить без экрана, проверить старт ядра, рабочий стол и ввод с клавиатуры
make clean  # удалить сборку
```

В окне QEMU щёлкните мышью, чтобы QEMU захватил указатель (Ctrl+Alt+G отпускает).
Часы на панели задач показывают время часов QEMU: с `-rtc base=localtime` (так
делает `make run`) это местное время, без него — UTC.

## Лицензия

MIT, см. [LICENSE](LICENSE). Шрифт Terminus Font (c) Dimitar Toshkov Zhekov,
SIL Open Font License 1.1, см. [fonts/LICENSE.terminus](fonts/LICENSE.terminus).
