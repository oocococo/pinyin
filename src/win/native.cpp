#define WIN32_LEAN_AND_MEAN

#include <windows.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <string>
#include <vector>

#ifndef RI_MOUSE_BUTTON_4_DOWN
#define RI_MOUSE_BUTTON_4_DOWN 0x0040
#endif

#ifndef RI_MOUSE_BUTTON_4_UP
#define RI_MOUSE_BUTTON_4_UP 0x0080
#endif

#ifndef RI_MOUSE_BUTTON_5_DOWN
#define RI_MOUSE_BUTTON_5_DOWN 0x0100
#endif

#ifndef RI_MOUSE_BUTTON_5_UP
#define RI_MOUSE_BUTTON_5_UP 0x0200
#endif

extern "C" {

enum {
  PAL_INPUT_EVENT_KEYBOARD = 1,
  PAL_INPUT_EVENT_MOUSE = 2,
};

enum {
  PAL_INPUT_STATUS_RELEASED = 0,
  PAL_INPUT_STATUS_PRESSED = 1,
};

typedef struct {
  int32_t event_type;
  int32_t status;
  uint32_t key_code;
  char buffer[64];
  uintptr_t buffer_len;
} PalInputEvent;

typedef void (*PalEventCallback)(PalInputEvent event);

}

static constexpr wchar_t PAL_WINDOW_CLASS[] = L"PalPinyinRawInputWindow";
static PalEventCallback PAL_CALLBACK = nullptr;
static bool PAL_LOG_EVENTS = false;

static bool native_event_logging_enabled() {
  const char *value = getenv("RIME_POC_NATIVE_LOG_EVENTS");
  if (value == nullptr) {
    return false;
  }

  return strcmp(value, "0") != 0 &&
         strcmp(value, "false") != 0 &&
         strcmp(value, "FALSE") != 0;
}

static void maybe_sleep(int32_t delay_ms) {
  if (delay_ms > 0) {
    Sleep((DWORD)delay_ms);
  }
}

static void dispatch_event(const PalInputEvent &event) {
  if (PAL_CALLBACK != nullptr) {
    PAL_CALLBACK(event);
  }
}

static HKL foreground_keyboard_layout() {
  HWND foreground = GetForegroundWindow();
  if (foreground != nullptr) {
    DWORD thread_id = GetWindowThreadProcessId(foreground, nullptr);
    HKL layout = GetKeyboardLayout(thread_id);
    if (layout != nullptr) {
      return layout;
    }
  }

  return GetKeyboardLayout(0);
}

static void copy_wide_text(PalInputEvent *event, const WCHAR *text, int text_len) {
  if (text_len <= 0) {
    return;
  }

  int bytes = WideCharToMultiByte(
      CP_UTF8,
      0,
      text,
      text_len,
      event->buffer,
      (int)sizeof(event->buffer) - 1,
      nullptr,
      nullptr);
  if (bytes <= 0) {
    return;
  }

  event->buffer[bytes] = '\0';
  event->buffer_len = (uintptr_t)bytes;
}

static bool is_keyboard_message(UINT message) {
  return message == WM_KEYDOWN ||
         message == WM_KEYUP ||
         message == WM_SYSKEYDOWN ||
         message == WM_SYSKEYUP;
}

static bool is_pressed_keyboard_message(UINT message) {
  return message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
}

static void process_keyboard_input(const RAWINPUT *raw) {
  if (!is_keyboard_message(raw->data.keyboard.Message)) {
    return;
  }

  if (raw->header.hDevice == nullptr) {
    if (PAL_LOG_EVENTS) {
      fprintf(stderr, "[rime-poc native] skipped synthetic keyboard event\n");
      fflush(stderr);
    }
    return;
  }

  PalInputEvent event = {};
  bool is_pressed = is_pressed_keyboard_message(raw->data.keyboard.Message);
  event.event_type = PAL_INPUT_EVENT_KEYBOARD;
  event.status = is_pressed ? PAL_INPUT_STATUS_PRESSED : PAL_INPUT_STATUS_RELEASED;
  event.key_code = (uint32_t)raw->data.keyboard.VKey;

  if (is_pressed) {
    BYTE keyboard_state[256] = {};
    if (GetKeyboardState(keyboard_state)) {
      WCHAR wide_buffer[32] = {};
      UINT flags = (1 << 2) | 1;
      int result = ToUnicodeEx(
          raw->data.keyboard.VKey,
          raw->data.keyboard.MakeCode,
          keyboard_state,
          wide_buffer,
          (int)(sizeof(wide_buffer) / sizeof(wide_buffer[0])) - 1,
          flags,
          foreground_keyboard_layout());
      if (result > 0) {
        copy_wide_text(&event, wide_buffer, result);
      }
    }
  }

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] raw key status=%d key=%u text=%s len=%llu\n",
            event.status,
            event.key_code,
            event.buffer_len > 0 ? event.buffer : "<empty>",
            (unsigned long long)event.buffer_len);
    fflush(stderr);
  }

  dispatch_event(event);
}

static void process_mouse_input(const RAWINPUT *raw) {
  USHORT flags = raw->data.mouse.usButtonFlags;
  const USHORT down_flags =
      RI_MOUSE_LEFT_BUTTON_DOWN |
      RI_MOUSE_RIGHT_BUTTON_DOWN |
      RI_MOUSE_MIDDLE_BUTTON_DOWN |
      RI_MOUSE_BUTTON_4_DOWN |
      RI_MOUSE_BUTTON_5_DOWN;
  const USHORT up_flags =
      RI_MOUSE_LEFT_BUTTON_UP |
      RI_MOUSE_RIGHT_BUTTON_UP |
      RI_MOUSE_MIDDLE_BUTTON_UP |
      RI_MOUSE_BUTTON_4_UP |
      RI_MOUSE_BUTTON_5_UP;

  PalInputEvent event = {};
  event.event_type = PAL_INPUT_EVENT_MOUSE;
  if ((flags & down_flags) != 0) {
    event.status = PAL_INPUT_STATUS_PRESSED;
  } else if ((flags & up_flags) != 0) {
    event.status = PAL_INPUT_STATUS_RELEASED;
  } else {
    return;
  }

  if (PAL_LOG_EVENTS) {
    fprintf(stderr, "[rime-poc native] raw mouse status=%d\n", event.status);
    fflush(stderr);
  }

  dispatch_event(event);
}

static LRESULT CALLBACK window_procedure(
    HWND window,
    UINT message,
    WPARAM wparam,
    LPARAM lparam) {
  (void)wparam;

  switch (message) {
  case WM_DESTROY:
    PostQuitMessage(0);
    return 0;

  case WM_INPUT: {
    UINT size = 0;
    if (GetRawInputData(
            (HRAWINPUT)lparam,
            RID_INPUT,
            nullptr,
            &size,
            sizeof(RAWINPUTHEADER)) == (UINT)-1) {
      return 0;
    }

    std::vector<BYTE> buffer(size);
    if (GetRawInputData(
            (HRAWINPUT)lparam,
            RID_INPUT,
            buffer.data(),
            &size,
            sizeof(RAWINPUTHEADER)) != size) {
      return 0;
    }

    RAWINPUT *raw = reinterpret_cast<RAWINPUT *>(buffer.data());
    if (raw->header.dwType == RIM_TYPEKEYBOARD) {
      process_keyboard_input(raw);
    } else if (raw->header.dwType == RIM_TYPEMOUSE) {
      process_mouse_input(raw);
    }

    return 0;
  }

  default:
    return DefWindowProcW(window, message, wparam, lparam);
  }
}

static HWND create_raw_input_window() {
  WNDCLASSEXW window_class = {};
  window_class.cbSize = sizeof(WNDCLASSEXW);
  window_class.lpfnWndProc = window_procedure;
  window_class.hInstance = GetModuleHandleW(nullptr);
  window_class.lpszClassName = PAL_WINDOW_CLASS;
  window_class.hCursor = LoadCursor(nullptr, IDC_ARROW);

  if (!RegisterClassExW(&window_class)) {
    DWORD error = GetLastError();
    if (error != ERROR_CLASS_ALREADY_EXISTS) {
      fprintf(stderr, "[rime-poc native] failed to register Raw Input window class: %lu\n", error);
      fflush(stderr);
      return nullptr;
    }
  }

  HWND window = CreateWindowExW(
      0,
      PAL_WINDOW_CLASS,
      L"rime-poc Raw Input Window",
      WS_OVERLAPPEDWINDOW,
      CW_USEDEFAULT,
      CW_USEDEFAULT,
      100,
      100,
      nullptr,
      nullptr,
      GetModuleHandleW(nullptr),
      nullptr);
  if (window == nullptr) {
    fprintf(stderr, "[rime-poc native] failed to create Raw Input window: %lu\n", GetLastError());
    fflush(stderr);
    return nullptr;
  }

  RAWINPUTDEVICE devices[2] = {};
  devices[0].usUsagePage = 0x01;
  devices[0].usUsage = 0x06;
  devices[0].dwFlags = RIDEV_NOLEGACY | RIDEV_INPUTSINK;
  devices[0].hwndTarget = window;

  devices[1].usUsagePage = 0x01;
  devices[1].usUsage = 0x02;
  devices[1].dwFlags = RIDEV_INPUTSINK;
  devices[1].hwndTarget = window;

  if (!RegisterRawInputDevices(devices, 2, sizeof(devices[0]))) {
    fprintf(stderr, "[rime-poc native] failed to register Raw Input devices: %lu\n", GetLastError());
    fflush(stderr);
    DestroyWindow(window);
    return nullptr;
  }

  return window;
}

static INPUT make_vkey_input(WORD vkey, bool key_up) {
  INPUT input = {};
  input.type = INPUT_KEYBOARD;
  input.ki.wVk = vkey;
  input.ki.dwFlags = key_up ? KEYEVENTF_KEYUP : 0;
  return input;
}

static INPUT make_unicode_input(WCHAR ch, bool key_up) {
  INPUT input = {};
  input.type = INPUT_KEYBOARD;
  input.ki.wScan = ch;
  input.ki.dwFlags = KEYEVENTF_UNICODE | (key_up ? KEYEVENTF_KEYUP : 0);
  return input;
}

static void send_inputs(std::vector<INPUT> *inputs) {
  if (inputs->empty()) {
    return;
  }

  SendInput((UINT)inputs->size(), inputs->data(), sizeof(INPUT));
}

extern "C" void pal_pinyin_start_event_loop(PalEventCallback callback) {
  PAL_CALLBACK = callback;
  PAL_LOG_EVENTS = native_event_logging_enabled();

  fprintf(stderr,
          "[rime-poc native] starting Win32 Raw Input event loop pid=%lu log_events=%s\n",
          GetCurrentProcessId(),
          PAL_LOG_EVENTS ? "true" : "false");
  fflush(stderr);

  HWND window = create_raw_input_window();
  if (window == nullptr) {
    return;
  }

  ShowWindow(window, SW_HIDE);

  MSG message = {};
  while (GetMessageW(&message, nullptr, 0, 0) > 0) {
    TranslateMessage(&message);
    DispatchMessageW(&message);
  }
}

extern "C" void pal_pinyin_inject_backspaces(uint32_t count, int32_t delay_ms) {
  fprintf(stderr,
          "[rime-poc native] injecting backspaces count=%u delay_ms=%d\n",
          count,
          delay_ms);
  fflush(stderr);

  if (delay_ms <= 0) {
    std::vector<INPUT> inputs;
    inputs.reserve((size_t)count * 2);
    for (uint32_t i = 0; i < count; i++) {
      inputs.push_back(make_vkey_input(VK_BACK, false));
      inputs.push_back(make_vkey_input(VK_BACK, true));
    }
    send_inputs(&inputs);
    return;
  }

  for (uint32_t i = 0; i < count; i++) {
    std::vector<INPUT> down = { make_vkey_input(VK_BACK, false) };
    send_inputs(&down);
    maybe_sleep(delay_ms);

    std::vector<INPUT> up = { make_vkey_input(VK_BACK, true) };
    send_inputs(&up);
    maybe_sleep(delay_ms);
  }
}

extern "C" void pal_pinyin_inject_string(const char *string, int32_t delay_ms) {
  if (string == nullptr) {
    return;
  }

  int required = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, string, -1, nullptr, 0);
  if (required <= 1) {
    return;
  }

  std::wstring wide((size_t)required, L'\0');
  int converted = MultiByteToWideChar(
      CP_UTF8,
      MB_ERR_INVALID_CHARS,
      string,
      -1,
      wide.data(),
      required);
  if (converted <= 1) {
    return;
  }

  if (!wide.empty() && wide.back() == L'\0') {
    wide.pop_back();
  }

  fprintf(stderr, "[rime-poc native] injecting unicode text delay_ms=%d\n", delay_ms);
  fflush(stderr);

  if (delay_ms <= 0) {
    std::vector<INPUT> inputs;
    inputs.reserve(wide.size() * 2);
    for (WCHAR ch : wide) {
      inputs.push_back(make_unicode_input(ch, false));
      inputs.push_back(make_unicode_input(ch, true));
    }
    send_inputs(&inputs);
    return;
  }

  for (WCHAR ch : wide) {
    std::vector<INPUT> down = { make_unicode_input(ch, false) };
    send_inputs(&down);
    maybe_sleep(delay_ms);

    std::vector<INPUT> up = { make_unicode_input(ch, true) };
    send_inputs(&up);
    maybe_sleep(delay_ms);
  }
}
