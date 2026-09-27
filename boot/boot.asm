; ==============================================================================
; EverOS - Низкоуровневый загрузчик (32-bit to 64-bit Long Mode Transition)
; ==============================================================================

global start
extern long_mode_start

section .text
bits 32
start:
    ; Инициализируем стек ядра
    mov esp, stack_top
    
    ; Сохраняем указатель на структуру multiboot2, переданную GRUB
    mov edi, ebx                

    ; Базовые аппаратные проверки процессора перед переходом
    call check_multiboot
    call check_cpuid
    call check_long_mode

    ; Безопасная инициализация таблиц страниц (Identity Mapping первых 4 GiB)
    call set_up_page_tables
    call enable_paging

    ; Загружаем 64-битную таблицу дескрипторов сегментов (GDT)
    lgdt [gdt64.pointer]
    
    ; Делаем дальний прыжок (Far Jump) в длинный 64-битный режим!
    jmp gdt64.code:long_mode_start

; ------------------------------------------------------------------------------
; Функция вывода критической ошибки на экран (VGA текстовый режим 0xb8000)
; al - символ кода ошибки ('0', '1', '2'), после чего процессор останавливается.
; ------------------------------------------------------------------------------
error:
    mov dword [0xb8000], 0x4f524f45   ; 'ER' на красном фоне
    mov dword [0xb8004], 0x4f3a4f52   ; 'R:' на красном фоне
    mov dword [0xb8008], 0x4f204f20   ; '  ' на красном фоне
    mov byte  [0xb800a], al           ; Код ошибки
    hlt

check_multiboot:
    cmp eax, 0x36d76289         ; Проверяем магическое число Multiboot2 от GRUB
    jne .no_multiboot
    ret
.no_multiboot:
    mov al, "0"                 ; Ошибка 0: Не Multiboot2 загрузчик
    jmp error

check_cpuid:
    ; Проверяем поддержку инструкции CPUID (флипаем 21-й бит в EFLAGS)
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .no_cpuid
    ret
.no_cpuid:
    mov al, "1"                 ; Ошибка 1: Процессор не поддерживает CPUID
    jmp error

check_long_mode:
    mov eax, 0x80000000         ; Запрос расширенных функций CPUID
    cpuid
    cmp eax, 0x80000001
    jb .no_long_mode
    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29           ; Проверяем бит Long Mode (LM-bit)
    jz .no_long_mode
    ret
.no_long_mode:
    mov al, "2"                 ; Ошибка 2: Процессор не 64-битный (нет Long Mode)
    jmp error

; ------------------------------------------------------------------------------
; Безопасное отображение страниц без использования деструктивной команды mul.
; Защищает регистр EDX от мусора, предотвращая тройное нарушение (Triple Fault)
; ------------------------------------------------------------------------------
set_up_page_tables:
    mov eax, p3_table
    or eax, 0b11                ; Флаги: present + writable
    mov [p4_table], eax

    ; Связываем 4 записи таблицы P3 со следующими четырьмя таблицами P2
    mov ecx, 0
.map_p3_table:
    mov eax, ecx
    shl eax, 12                 ; ecx * 4096 байт (сдвиг без разрушения edx)
    add eax, p2_tables
    or eax, 0b11                ; Флаги: present + writable
    mov [p3_table + ecx * 8], eax
    inc ecx
    cmp ecx, 4
    jne .map_p3_table

    ; Заполняем 4 таблицы P2 (всего 2048 записей по 2 МиБ каждая = 4 ГиБ памяти)
    mov ecx, 0
.map_p2_table:
    mov eax, ecx
    shl eax, 21                 ; ecx * 2 MiB (вычисляем физический адрес страницы)
    or eax, 0b10000011          ; Флаги: present + writable + huge page (2 MiB)
    mov [p2_tables + ecx * 8], eax
    inc ecx
    cmp ecx, 512 * 4            ; Итерируем по всем 4-м таблицам P2
    jne .map_p2_table
    ret

enable_paging:
    ; Загружаем адрес корневой таблицы PML4 (p4_table) в регистр управления CR3
    mov eax, p4_table
    mov cr3, eax

    ; Включаем расширение физического адреса (PAE) и поддержку инструкций SSE
    mov eax, cr4                
    or eax, (1 << 5) | (1 << 9) | (1 << 10) ; PAE + OSFXSR + OSXMMEXCPT
    mov cr4, eax

    ; Читаем модель-специфичный регистр EFER MSR для активации Long Mode
    mov ecx, 0xC0000080         
    rdmsr                       ; EDX теперь чист от мусора, процессор стабилен!
    or eax, 1 << 8              ; Устанавливаем бит Long Mode Active (LME)
    wrmsr

    ; Включаем страничную адресацию (Paging) в регистре CR0
    mov eax, cr0                
    and eax, ~(1 << 2)          ; Сбрасываем EM (эмуляцию копроцессора), необходимо для SSE
    or eax, (1 << 31) | (1 << 1) ; Включаем PG (Paging) и MP (Monitor Coprocessor)
    mov cr0, eax
    ret

; ------------------------------------------------------------------------------
; Глобальная таблица дескрипторов (GDT) для перехода в 64-битный режим
; ------------------------------------------------------------------------------
section .rodata
gdt64:
    dq 0                                              ; Нулевой дескриптор (Null Descriptor)
.code: equ $ - gdt64
    dq (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53)  ; Дескриптор 64-битного кода ядра
.pointer:
    dw $ - gdt64 - 1
    dq gdt64

; ------------------------------------------------------------------------------
; Секция неинициализированных данных (BSS) с жестким выравниванием страниц
; ------------------------------------------------------------------------------
section .bss
align 4096
p4_table:
    resb 4096
p3_table:
    resb 4096
p2_tables:
    resb 4096 * 4
stack_bottom:
    ; Выделяем честный 1 МиБ под стек ядра (для тяжелого JS, графики и шрифтов)
    resb 4096 * 256              
stack_top:
