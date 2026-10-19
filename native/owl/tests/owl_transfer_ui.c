/* Real local IPC with a fake serial queue: no radio or Wintun required. */
#include "../io_esp.c"
#define CHECK(expr) do { if (!(expr)) { fprintf(stderr, "line %d: %s\n", __LINE__, #expr); return 1; } } while (0)
static HANDLE connect_ui(const wchar_t *name) {
    ULONGLONG deadline = GetTickCount64() + 2000;
    do {
        HANDLE pipe = CreateFileW(name, 0x12019b, 0, NULL, OPEN_EXISTING, SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, NULL);
        if (pipe != INVALID_HANDLE_VALUE) return pipe;
        Sleep(10);
    } while (GetTickCount64() < deadline);
    return INVALID_HANDLE_VALUE;
}
static int exchange(const wchar_t *name, const uint8_t *data, DWORD size, uint8_t expected) {
    HANDLE pipe = connect_ui(name);
    if (pipe == INVALID_HANDLE_VALUE) return 0;
    DWORD n = 0; uint8_t reply = 0xfe, ack = 0xff;
    int ok = WriteFile(pipe, data, size, &n, NULL) && n == size
        && ReadFile(pipe, &reply, 1, &n, NULL) && n == 1 && reply == expected;
    WriteFile(pipe, &ack, 1, &n, NULL);
    CloseHandle(pipe);
    return ok;
}
int main(void) {
    wchar_t name[128];
    swprintf(name, 128, L"\\\\.\\pipe\\omdrop-ui-test-%lu", GetCurrentProcessId());
    InitializeCriticalSection(&radio.tx_lock);
    radio.tx_wake = CreateEventW(NULL, FALSE, FALSE, NULL);
    radio.reply_event = CreateEventW(NULL, TRUE, FALSE, NULL);
    radio.txq = calloc(TXQ_LEN, sizeof *radio.txq);
    CHECK(radio.tx_wake && radio.reply_event && radio.txq);
    HANDLE worker = CreateThread(NULL, 0, transfer_ui_main, name, 0, NULL);
    CHECK(worker);
    uint8_t request[1 + UI_FRAME] = {0};
    request[1] = 1; request[3] = 1; request[9] = 123;
    CHECK(exchange(name, request, sizeof request, 0));
    CHECK(radio.tx_count == 1);
    uint8_t decoded[COBS_MAX];
    memcpy(decoded, radio.txq[0].data, radio.txq[0].len);
    int n = cobs_decode(decoded, radio.txq[0].len - 1);
    CHECK(n == UI_FRAME + 3 && decoded[0] == MSG_TRANSFER);
    CHECK(!memcmp(decoded + 1, request + 1, UI_FRAME));
    request[3] = 255;
    CHECK(exchange(name, request, sizeof request, 1));
    CHECK(radio.tx_count == 1);
    /* A partial frame is rejected within a bounded time and cannot monopolize the pipe. */
    ULONGLONG started = GetTickCount64();
    CHECK(exchange(name, request, 1, 1));
    CHECK(GetTickCount64() - started < 2000);
    request[3] = 3;
    CHECK(exchange(name, request, sizeof request, 0));
    CHECK(radio.tx_count == 2);
    radio.stop = 1;
    CHECK(WaitForSingleObject(worker, 2000) == WAIT_OBJECT_0);
    CloseHandle(worker); CloseHandle(radio.tx_wake); CloseHandle(radio.reply_event);
    free(radio.txq); DeleteCriticalSection(&radio.tx_lock);
    return 0;
}
