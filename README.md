# EverOS

EverOS — операционная система для x86_64 на ASM и Rust.

Загрузчик GRUB (multiboot2) передаёт управление коду на ассемблере, тот
проверяет процессор, включает страничную память и 64-битный режим (long mode)
и вызывает ядро на Rust. Сейчас ядро выводит «EverOS» на экран и в
последовательный порт.

## Структура

| Путь | Что там |
| --- | --- |
| `boot/multiboot_header.asm` | заголовок multiboot2 для GRUB |
| `boot/boot.asm` | 32-битный старт: проверки CPU, таблицы страниц, переход в long mode |
| `boot/long_mode.asm` | 64-битная точка входа, вызывает `kernel_main` |
| `kernel/` | ядро на Rust (`no_std`, цель `x86_64-unknown-none`) |
| `linker.ld` | скрипт линкера, ядро грузится по адресу 1 МиБ |
| `iso/boot/grub/grub.cfg` | конфиг GRUB для загрузочного ISO |
| `scripts/boot-test.sh` | запуск в QEMU без экрана и проверка, что ядро стартовало |

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
make test   # загрузить без экрана и проверить вывод ядра
make clean  # удалить сборку
```

## Лицензия

MIT, см. [LICENSE](LICENSE).
