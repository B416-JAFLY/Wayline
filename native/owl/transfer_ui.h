/* Local, bounded UI-only IPC. Included after radio_request/radio_queue in io_esp.c. */
#include <sddl.h>
#define UI_HEADER 32
#define UI_FRAME (UI_HEADER + 160 * 14 / 2)

static int ui_valid_frame(const uint8_t *p)
{
	uint64_t id, done, total;
	memcpy(&id, p + 8, 8); memcpy(&done, p + 16, 8); memcpy(&total, p + 24, 8);
	return p[0] == 1 && p[1] <= 1 && p[2] <= 7 && p[3] <= 5 && p[4] <= 1 && !p[5]
	       && (!p[2] || id) && (!p[4] || (total && done <= total));
}

static int ui_finish_io(HANDLE pipe, OVERLAPPED *ov, BOOL immediate, DWORD *count, DWORD timeout)
{
	if (immediate) return 1;
	if (GetLastError() != ERROR_IO_PENDING) return 0;
	DWORD elapsed = 0;
	while (!radio.stop && !radio.failed && elapsed < timeout) {
		if (WaitForSingleObject(ov->hEvent, 50) == WAIT_OBJECT_0)
			return GetOverlappedResult(pipe, ov, count, FALSE) != 0;
		elapsed += 50;
	}
	CancelIoEx(pipe, ov);
	GetOverlappedResult(pipe, ov, count, TRUE); /* Keep overlapped storage alive until cancellation completes. */
	return 0;
}

static int ui_read_exact(HANDLE pipe, OVERLAPPED *ov, uint8_t *data, DWORD size)
{
	ULONGLONG deadline = GetTickCount64() + 500;
	while (size && GetTickCount64() < deadline) {
		DWORD got = 0;
		ResetEvent(ov->hEvent);
		BOOL ok = ReadFile(pipe, data, size, &got, ov);
		if (!ui_finish_io(pipe, ov, ok, &got, 500) || !got) return 0;
		data += got; size -= got;
	}
	return size == 0;
}

static DWORD WINAPI transfer_ui_main(void *arg)
{
	const wchar_t *name = arg ? (const wchar_t *)arg : L"\\\\.\\pipe\\omdrop-radio-ui";
	PSECURITY_DESCRIPTOR descriptor = NULL;
	/* Local users may submit UI state, never create a competing server or access radio commands. */
	if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
	        L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;AU)",
	        SDDL_REVISION_1, &descriptor, NULL)) return 1;
	SECURITY_ATTRIBUTES sa = {sizeof sa, descriptor, FALSE};
	HANDLE pipe = CreateNamedPipeW(name, PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
	        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
	        1, 64, UI_FRAME + 1, 500, &sa);
	LocalFree(descriptor);
	if (pipe == INVALID_HANDLE_VALUE) { log_warn("transfer UI pipe unavailable (%lu)", GetLastError()); return 1; }
	OVERLAPPED ov = {.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL)};
	if (!ov.hEvent) { CloseHandle(pipe); return 1; }
	while (!radio.stop && !radio.failed) {
		DWORD count = 0;
		ResetEvent(ov.hEvent);
		BOOL connected = ConnectNamedPipe(pipe, &ov);
		if (!connected && GetLastError() == ERROR_PIPE_CONNECTED) connected = TRUE;
		if (!ui_finish_io(pipe, &ov, connected, &count, 1000)) { DisconnectNamedPipe(pipe); continue; }
		uint8_t op, request[UI_FRAME], response[1 + UI_HEADER] = {1};
		DWORD response_len = 1;
		if (ui_read_exact(pipe, &ov, &op, 1)) {
			if (op == 0 && ui_read_exact(pipe, &ov, request, UI_FRAME) && ui_valid_frame(request)) {
				response[0] = radio_queue(MSG_TRANSFER, request, UI_FRAME, NULL, 0) == 0 ? 0 : 1;
			} else if (op == 1 && ui_read_exact(pipe, &ov, request, 1) && request[0] <= 1) {
				/* This thread starts after all synchronous startup commands. It is the only later requester. */
				int n = radio_request(MSG_TRANSFER_STATUS, request, 1, MSG_TRANSFER_RESP, 500);
				if (n == UI_HEADER) { response[0] = 0; memcpy(response + 1, radio.reply, UI_HEADER); response_len += UI_HEADER; }
			}
			ResetEvent(ov.hEvent);
			BOOL ok = WriteFile(pipe, response, response_len, &count, &ov);
			if (ui_finish_io(pipe, &ov, ok, &count, 500) && count == response_len) {
				/* Client acknowledges reading the reply before disconnect can discard buffered bytes. */
				ui_read_exact(pipe, &ov, &op, 1);
			}
		}
		DisconnectNamedPipe(pipe);
		Sleep(10); /* Bound UI traffic even from an aggressive local client. */
	}
	CloseHandle(ov.hEvent); CloseHandle(pipe);
	return 0;
}
