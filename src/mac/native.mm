#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

#include <algorithm>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <string>
#include <unistd.h>
#include <vector>

extern "C" {

enum {
  PAL_INPUT_EVENT_KEYBOARD = 1,
  PAL_INPUT_EVENT_MOUSE = 2,
  PAL_INPUT_EVENT_CONTEXT = 3,
};

enum {
  PAL_INPUT_STATUS_RELEASED = 0,
  PAL_INPUT_STATUS_PRESSED = 1,
};

typedef struct {
  int32_t event_type;
  int32_t status;
  uint32_t key_code;
  uint32_t modifier_flags;
  char buffer[64];
  uintptr_t buffer_len;
  char source_buffer[256];
  uintptr_t source_buffer_len;
} PalInputEvent;

typedef int32_t (*PalEventCallback)(PalInputEvent event);

}

static constexpr CGFloat PAL_EVENT_MARKER = -27469;
static PalEventCallback PAL_CALLBACK = nullptr;
static id PAL_WORKSPACE_OBSERVER = nil;
static id PAL_INPUT_SOURCE_OBSERVER = nil;
static NSPanel *PAL_CANDIDATE_PANEL = nil;
static NSView *PAL_CANDIDATE_CONTENT = nil;
static CFMachPortRef PAL_EVENT_TAP = nullptr;
static CFRunLoopSourceRef PAL_EVENT_TAP_SOURCE = nullptr;
static bool PAL_LOG_EVENTS = false;
static bool PAL_REWRITE_TRANSACTION_ACTIVE = false;
static uint64_t PAL_REWRITE_TRANSACTION_GENERATION = 0;
static std::vector<PalInputEvent> PAL_REWRITE_BUFFERED_EVENTS;
static std::vector<uint32_t> PAL_REWRITE_SWALLOWED_KEYUPS;
static bool PAL_REWRITE_OPERATION_RUNNING = false;
static std::vector<std::string> PAL_SHIFTED_KEY_TEXT(128);
static std::vector<std::string> PAL_CAPS_LOCK_KEY_TEXT(128);
static std::vector<std::string> PAL_SHIFTED_CAPS_LOCK_KEY_TEXT(128);

struct PalRewriteOperation {
  uint32_t delete_count;
  std::string replacement_text;
  int32_t delay_ms;
};

static std::vector<PalRewriteOperation> PAL_REWRITE_OPERATION_QUEUE;

enum {
  PAL_INPUT_MODIFIER_COMMAND = 1 << 0,
  PAL_INPUT_MODIFIER_CONTROL = 1 << 1,
  PAL_INPUT_MODIFIER_OPTION = 1 << 2,
  PAL_INPUT_MODIFIER_SHIFT = 1 << 3,
  PAL_INPUT_MODIFIER_BUFFERED_REPLAY = 1 << 4,
  PAL_INPUT_MODIFIER_REWRITE_ACTIVE = 1 << 5,
};

static bool native_event_logging_enabled() {
  const char *value = getenv("RIME_POC_NATIVE_LOG_EVENTS");
  if (value == nullptr) {
    return false;
  }
  return strcmp(value, "0") != 0 && strcmp(value, "false") != 0 && strcmp(value, "FALSE") != 0;
}

static void post_key(uint16_t key_code, bool down, int32_t delay_ms) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, down);
  CGEventSetLocation(event, CGPointMake(PAL_EVENT_MARKER, 0));
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
  usleep(delay_ms * 1000);
}

static uint32_t pal_modifier_flags_from_cg(CGEventFlags flags) {
  uint32_t result = 0;
  if ((flags & kCGEventFlagMaskCommand) != 0) {
    result |= PAL_INPUT_MODIFIER_COMMAND;
  }
  if ((flags & kCGEventFlagMaskControl) != 0) {
    result |= PAL_INPUT_MODIFIER_CONTROL;
  }
  if ((flags & kCGEventFlagMaskAlternate) != 0) {
    result |= PAL_INPUT_MODIFIER_OPTION;
  }
  if ((flags & kCGEventFlagMaskShift) != 0) {
    result |= PAL_INPUT_MODIFIER_SHIFT;
  }
  return result;
}

static void copy_current_input_source_fingerprint(PalInputEvent *input);

static void pal_populate_keyboard_layout_text(
    const UCKeyboardLayout *layout,
    UInt32 keyboard_type,
    UInt32 carbon_modifiers,
    std::vector<std::string> *table) {
  for (std::string &text : *table) {
    text.clear();
  }

  for (UInt16 key_code = 0; key_code < table->size(); key_code++) {
    UInt32 dead_key_state = 0;
    UniChar chars[32] = {};
    UniCharCount actual_length = 0;
    OSStatus status = UCKeyTranslate(
        layout,
        key_code,
        kUCKeyActionDown,
        (carbon_modifiers >> 8) & 0xff,
        keyboard_type,
        kUCKeyTranslateNoDeadKeysBit,
        &dead_key_state,
        (UniCharCount)(sizeof(chars) / sizeof(chars[0])),
        &actual_length,
        chars);
    if (status != noErr || actual_length == 0) {
      continue;
    }

    CFStringRef string = CFStringCreateWithCharacters(
        kCFAllocatorDefault,
        chars,
        actual_length);
    if (string != nullptr) {
      char buffer[64] = {};
      if (CFStringGetCString(string, buffer, sizeof(buffer), kCFStringEncodingUTF8)) {
        (*table)[key_code] = buffer;
      }
      CFRelease(string);
    }
  }
}

static void pal_refresh_keyboard_layout() {
  PAL_SHIFTED_KEY_TEXT.assign(PAL_SHIFTED_KEY_TEXT.size(), std::string());
  PAL_CAPS_LOCK_KEY_TEXT.assign(PAL_CAPS_LOCK_KEY_TEXT.size(), std::string());
  PAL_SHIFTED_CAPS_LOCK_KEY_TEXT.assign(
      PAL_SHIFTED_CAPS_LOCK_KEY_TEXT.size(),
      std::string());

  TISInputSourceRef source = TISCopyCurrentKeyboardLayoutInputSource();
  if (source == nullptr) {
    return;
  }

  CFDataRef layout_data = (CFDataRef)TISGetInputSourceProperty(
      source,
      kTISPropertyUnicodeKeyLayoutData);
  if (layout_data == nullptr) {
    CFRelease(source);
    return;
  }

  const UCKeyboardLayout *layout =
      (const UCKeyboardLayout *)CFDataGetBytePtr(layout_data);
  if (layout == nullptr) {
    CFRelease(source);
    return;
  }

  UInt32 keyboard_type = LMGetKbdType();
  pal_populate_keyboard_layout_text(
      layout,
      keyboard_type,
      shiftKey,
      &PAL_SHIFTED_KEY_TEXT);
  pal_populate_keyboard_layout_text(
      layout,
      keyboard_type,
      alphaLock,
      &PAL_CAPS_LOCK_KEY_TEXT);
  pal_populate_keyboard_layout_text(
      layout,
      keyboard_type,
      shiftKey | alphaLock,
      &PAL_SHIFTED_CAPS_LOCK_KEY_TEXT);
  CFRelease(source);
}

static const std::string *modified_keyboard_layout_text(CGEventRef event) {
  int64_t key_code = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode);
  if (key_code < 0 || (size_t)key_code >= PAL_SHIFTED_KEY_TEXT.size()) {
    return nullptr;
  }

  CGEventFlags flags = CGEventGetFlags(event);
  bool shifted = (flags & kCGEventFlagMaskShift) != 0;
  bool caps_locked = (flags & kCGEventFlagMaskAlphaShift) != 0;
  const std::vector<std::string> *table = nullptr;
  if (shifted && caps_locked) {
    table = &PAL_SHIFTED_CAPS_LOCK_KEY_TEXT;
  } else if (shifted) {
    table = &PAL_SHIFTED_KEY_TEXT;
  } else if (caps_locked) {
    table = &PAL_CAPS_LOCK_KEY_TEXT;
  } else {
    return nullptr;
  }

  const std::string &text = (*table)[(size_t)key_code];
  return text.empty() ? nullptr : &text;
}

static bool pal_rewrite_is_busy() {
  return PAL_REWRITE_TRANSACTION_ACTIVE ||
      PAL_REWRITE_OPERATION_RUNNING ||
      !PAL_REWRITE_OPERATION_QUEUE.empty();
}

static void pal_mark_rewrite_active(PalInputEvent *input) {
  if (pal_rewrite_is_busy()) {
    input->modifier_flags |= PAL_INPUT_MODIFIER_REWRITE_ACTIVE;
  }
}

static void dispatch_context_event(const char *reason) {
  if (PAL_CALLBACK == nullptr) {
    return;
  }

  PalInputEvent input = {};
  input.event_type = PAL_INPUT_EVENT_CONTEXT;
  input.status = PAL_INPUT_STATUS_PRESSED;
  input.key_code = 0;
  strncpy(input.buffer, reason, sizeof(input.buffer) - 1);
  input.buffer[sizeof(input.buffer) - 1] = '\0';
  input.buffer_len = strlen(input.buffer);
  copy_current_input_source_fingerprint(&input);
  pal_mark_rewrite_active(&input);

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] context event reason=%s source=%s\n",
            reason,
            input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
    fflush(stderr);
  }

  (void)PAL_CALLBACK(input);
}

extern "C" bool pal_pinyin_is_accessibility_trusted(bool prompt) {
  const void *keys[] = { kAXTrustedCheckOptionPrompt };
  const void *values[] = { prompt ? kCFBooleanTrue : kCFBooleanFalse };
  CFDictionaryRef options = CFDictionaryCreate(
      kCFAllocatorDefault,
      keys,
      values,
      1,
      &kCFCopyStringDictionaryKeyCallBacks,
      &kCFTypeDictionaryValueCallBacks);
  bool trusted = AXIsProcessTrustedWithOptions(options);
  CFRelease(options);
  return trusted;
}

extern "C" bool pal_pinyin_has_input_monitoring_access() {
  if (@available(macOS 10.15, *)) {
    return CGPreflightListenEventAccess();
  }
  return true;
}

extern "C" bool pal_pinyin_request_input_monitoring_access() {
  if (@available(macOS 10.15, *)) {
    return CGRequestListenEventAccess();
  }
  return true;
}

static void copy_cf_string_to_buffer(
    CFStringRef string,
    char *buffer,
    uintptr_t *buffer_len,
    size_t buffer_capacity) {
  if (string == nullptr || buffer == nullptr || buffer_len == nullptr || buffer_capacity == 0) {
    return;
  }

  if (CFStringGetCString(
          string,
          buffer,
          buffer_capacity,
          kCFStringEncodingUTF8)) {
    buffer[buffer_capacity - 1] = '\0';
    *buffer_len = strlen(buffer);
  }
}

static void copy_string_to_input_buffer(PalInputEvent *input, CFStringRef string) {
  copy_cf_string_to_buffer(
      string,
      input->buffer,
      &input->buffer_len,
      sizeof(input->buffer));
}

static void copy_current_input_source_fingerprint(PalInputEvent *input) {
  TISInputSourceRef source = TISCopyCurrentKeyboardInputSource();
  if (source == nullptr) {
    return;
  }

  CFStringRef source_id = (CFStringRef)TISGetInputSourceProperty(
      source,
      kTISPropertyInputSourceID);
  CFStringRef mode_id = (CFStringRef)TISGetInputSourceProperty(
      source,
      kTISPropertyInputModeID);
  CFStringRef source_type = (CFStringRef)TISGetInputSourceProperty(
      source,
      kTISPropertyInputSourceType);
  CFBooleanRef ascii_capable = (CFBooleanRef)TISGetInputSourceProperty(
      source,
      kTISPropertyInputSourceIsASCIICapable);

  CFStringRef empty = CFSTR("");
  CFStringRef source_id_value = source_id != nullptr ? source_id : empty;
  CFStringRef mode_id_value = mode_id != nullptr ? mode_id : empty;
  CFStringRef source_type_value = source_type != nullptr ? source_type : empty;
  int ascii_capable_value = ascii_capable == kCFBooleanTrue ? 1 : 0;

  CFStringRef fingerprint = CFStringCreateWithFormat(
      kCFAllocatorDefault,
      nullptr,
      CFSTR("source=%@|mode=%@|type=%@|ascii=%d"),
      source_id_value,
      mode_id_value,
      source_type_value,
      ascii_capable_value);
  if (fingerprint != nullptr) {
    copy_cf_string_to_buffer(
        fingerprint,
        input->source_buffer,
        &input->source_buffer_len,
        sizeof(input->source_buffer));
    CFRelease(fingerprint);
  }
  CFRelease(source);
}

static void copy_cg_event_text(PalInputEvent *input, CGEventRef event) {
  UniChar chars[32] = {};
  UniCharCount actual_length = 0;
  CGEventKeyboardGetUnicodeString(
      event,
      (UniCharCount)(sizeof(chars) / sizeof(chars[0])),
      &actual_length,
      chars);

  if (actual_length == 0) {
    return;
  }

  CFStringRef string = CFStringCreateWithCharacters(
      kCFAllocatorDefault,
      chars,
      actual_length);
  CGEventFlags flags = CGEventGetFlags(event);
  if ((flags & (kCGEventFlagMaskShift | kCGEventFlagMaskAlphaShift)) != 0 &&
      actual_length == 1 && chars[0] < 128) {
    const std::string *modified = modified_keyboard_layout_text(event);
    if (modified != nullptr) {
      CFRelease(string);
      size_t length = std::min(modified->size(), sizeof(input->buffer) - 1);
      memcpy(input->buffer, modified->data(), length);
      input->buffer[length] = '\0';
      input->buffer_len = length;
      return;
    }
  }
  copy_string_to_input_buffer(input, string);
  CFRelease(string);
}

static bool pal_input_has_text_modifier(PalInputEvent input) {
  return (input.modifier_flags &
          (PAL_INPUT_MODIFIER_COMMAND | PAL_INPUT_MODIFIER_CONTROL | PAL_INPUT_MODIFIER_OPTION)) != 0;
}

static bool pal_take_swallowed_keyup(uint32_t key_code) {
  for (auto it = PAL_REWRITE_SWALLOWED_KEYUPS.begin();
       it != PAL_REWRITE_SWALLOWED_KEYUPS.end();
       ++it) {
    if (*it == key_code) {
      PAL_REWRITE_SWALLOWED_KEYUPS.erase(it);
      return true;
    }
  }
  return false;
}

static void pal_remember_swallowed_keyup(uint32_t key_code) {
  for (uint32_t existing : PAL_REWRITE_SWALLOWED_KEYUPS) {
    if (existing == key_code) {
      return;
    }
  }
  PAL_REWRITE_SWALLOWED_KEYUPS.push_back(key_code);
}

static void pal_forget_swallowed_keyup(uint32_t key_code) {
  for (auto it = PAL_REWRITE_SWALLOWED_KEYUPS.begin();
       it != PAL_REWRITE_SWALLOWED_KEYUPS.end();) {
    if (*it == key_code) {
      it = PAL_REWRITE_SWALLOWED_KEYUPS.erase(it);
    } else {
      ++it;
    }
  }
}

static bool pal_maybe_buffer_rewrite_event(PalInputEvent input) {
  if (input.event_type != PAL_INPUT_EVENT_KEYBOARD) {
    return false;
  }

  if (input.status == PAL_INPUT_STATUS_RELEASED) {
    return pal_take_swallowed_keyup(input.key_code);
  }

  if (!PAL_REWRITE_TRANSACTION_ACTIVE) {
    return false;
  }

  bool is_backspace = input.key_code == 0x33;
  if ((input.buffer_len == 0 && !is_backspace) || pal_input_has_text_modifier(input)) {
    return false;
  }

  PAL_REWRITE_BUFFERED_EVENTS.push_back(input);
  pal_remember_swallowed_keyup(input.key_code);
  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] rewrite transaction buffered key=%u chars=%s len=%lu\n",
            input.key_code,
            input.buffer_len > 0 ? input.buffer : "<empty>",
            (unsigned long)input.buffer_len);
    fflush(stderr);
  }
  return true;
}

static void pal_replay_rewrite_buffered_events(std::vector<PalInputEvent> events) {
  if (events.empty() || PAL_CALLBACK == nullptr) {
    return;
  }

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] replaying %lu rewrite transaction buffered events\n",
            (unsigned long)events.size());
    fflush(stderr);
  }

  for (size_t i = 0; i < events.size(); i++) {
    PalInputEvent input = events[i];
    input.modifier_flags |= PAL_INPUT_MODIFIER_BUFFERED_REPLAY;
    (void)PAL_CALLBACK(input);
    if (PAL_REWRITE_TRANSACTION_ACTIVE) {
      size_t remaining_count = events.size() - i - 1;
      if (remaining_count > 0) {
        PAL_REWRITE_BUFFERED_EVENTS.insert(
            PAL_REWRITE_BUFFERED_EVENTS.end(),
            events.begin() + i + 1,
            events.end());
      }
      if (PAL_LOG_EVENTS) {
        fprintf(stderr,
                "[rime-poc native] paused replay for nested rewrite transaction remaining=%lu\n",
                (unsigned long)remaining_count);
        fflush(stderr);
      }
      return;
    }
  }
}

static void pal_post_backspaces_now(uint32_t count, int32_t delay_ms) {
  for (uint32_t i = 0; i < count; i++) {
    post_key(0x33, true, delay_ms);
    post_key(0x33, false, delay_ms);
  }
}

static void pal_post_unicode_text_now(const std::string &text, int32_t delay_ms) {
  if (text.empty()) {
    return;
  }

  NSString *ns_string = [NSString stringWithUTF8String:text.c_str()];
  if (ns_string == nil) {
    return;
  }

  CFStringRef cf_string = (__bridge CFStringRef)ns_string;
  std::vector<UniChar> buffer(ns_string.length);
  CFStringGetCharacters(cf_string, CFRangeMake(0, ns_string.length), buffer.data());

  size_t index = 0;
  while (index < buffer.size()) {
    size_t chunk_size = 20;
    if (index + chunk_size > buffer.size()) {
      chunk_size = buffer.size() - index;
    }

    CGEventRef keydown = CGEventCreateKeyboardEvent(NULL, 0x31, true);
    CGEventSetLocation(keydown, CGPointMake(PAL_EVENT_MARKER, 0));
    CGEventKeyboardSetUnicodeString(keydown, (UniCharCount)chunk_size, buffer.data() + index);
    CGEventPost(kCGHIDEventTap, keydown);
    CFRelease(keydown);
    usleep(delay_ms * 1000);

    CGEventRef keyup = CGEventCreateKeyboardEvent(NULL, 0x31, false);
    CGEventSetLocation(keyup, CGPointMake(PAL_EVENT_MARKER, 0));
    CGEventPost(kCGHIDEventTap, keyup);
    CFRelease(keyup);
    usleep(delay_ms * 1000);

    index += chunk_size;
  }
}

static void pal_start_next_rewrite_operation();

static void pal_finish_current_rewrite_operation(uint64_t generation) {
  dispatch_after(
      dispatch_time(DISPATCH_TIME_NOW, 80 * NSEC_PER_MSEC),
      dispatch_get_main_queue(),
      ^{
        @autoreleasepool {
          if (!PAL_REWRITE_TRANSACTION_ACTIVE ||
              generation != PAL_REWRITE_TRANSACTION_GENERATION) {
            return;
          }

          PAL_REWRITE_TRANSACTION_ACTIVE = false;
          PAL_REWRITE_OPERATION_RUNNING = false;
          PAL_REWRITE_SWALLOWED_KEYUPS.clear();
          std::vector<PalInputEvent> events = PAL_REWRITE_BUFFERED_EVENTS;
          PAL_REWRITE_BUFFERED_EVENTS.clear();
          if (PAL_LOG_EVENTS) {
            fprintf(stderr,
                    "[rime-poc native] rewrite operation finish generation=%llu buffered=%lu queued=%lu\n",
                    (unsigned long long)generation,
                    (unsigned long)events.size(),
                    (unsigned long)PAL_REWRITE_OPERATION_QUEUE.size());
            fflush(stderr);
          }
          pal_replay_rewrite_buffered_events(events);
          if (!PAL_REWRITE_TRANSACTION_ACTIVE) {
            pal_start_next_rewrite_operation();
          }
        }
      });
}

static void pal_start_next_rewrite_operation() {
  if (PAL_REWRITE_OPERATION_RUNNING) {
    return;
  }
  if (PAL_REWRITE_OPERATION_QUEUE.empty()) {
    return;
  }

  PalRewriteOperation operation = PAL_REWRITE_OPERATION_QUEUE.front();
  PAL_REWRITE_OPERATION_QUEUE.erase(PAL_REWRITE_OPERATION_QUEUE.begin());
  PAL_REWRITE_OPERATION_RUNNING = true;
  if (!PAL_REWRITE_TRANSACTION_ACTIVE) {
    PAL_REWRITE_TRANSACTION_ACTIVE = true;
    PAL_REWRITE_TRANSACTION_GENERATION += 1;
    PAL_REWRITE_BUFFERED_EVENTS.clear();
    PAL_REWRITE_SWALLOWED_KEYUPS.clear();
  }
  uint64_t generation = PAL_REWRITE_TRANSACTION_GENERATION;
  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] rewrite operation begin generation=%llu delete=%u replacement_len=%lu queued=%lu\n",
            (unsigned long long)generation,
            operation.delete_count,
            (unsigned long)operation.replacement_text.size(),
            (unsigned long)PAL_REWRITE_OPERATION_QUEUE.size());
    fflush(stderr);
  }

  pal_post_backspaces_now(operation.delete_count, operation.delay_ms);
  pal_post_unicode_text_now(operation.replacement_text, operation.delay_ms);
  pal_finish_current_rewrite_operation(generation);
}

static bool dispatch_cg_event(CGEventType type, CGEventRef event) {
  if (PAL_CALLBACK == nullptr) {
    return false;
  }

  CGPoint location = CGEventGetLocation(event);
  if (fabs(location.x - PAL_EVENT_MARKER) < 0.001) {
    if (PAL_LOG_EVENTS) {
      fprintf(stderr, "[rime-poc native] skipped marked/self CGEvent type=%u\n", type);
      fflush(stderr);
    }
    return false;
  }

  PalInputEvent input = {};
  if (type == kCGEventKeyDown || type == kCGEventKeyUp || type == kCGEventFlagsChanged) {
    CGEventFlags flags = CGEventGetFlags(event);
    input.event_type = PAL_INPUT_EVENT_KEYBOARD;
    input.status = type == kCGEventKeyUp
        ? PAL_INPUT_STATUS_RELEASED
        : PAL_INPUT_STATUS_PRESSED;
    input.key_code = (uint32_t)CGEventGetIntegerValueField(
        event,
        kCGKeyboardEventKeycode);
    input.modifier_flags = pal_modifier_flags_from_cg(flags);
    if (type == kCGEventFlagsChanged && (flags & kCGEventFlagMaskShift) == 0) {
      input.status = PAL_INPUT_STATUS_RELEASED;
    }
    if (type != kCGEventFlagsChanged) {
      copy_cg_event_text(&input, event);
    }
    copy_current_input_source_fingerprint(&input);
    pal_mark_rewrite_active(&input);

    if (PAL_LOG_EVENTS) {
      fprintf(stderr,
              "[rime-poc native] cgevent key type=%u status=%d key=%u modifiers=%u chars=%s len=%lu source=%s\n",
              type,
              input.status,
              input.key_code,
              input.modifier_flags,
              input.buffer_len > 0 ? input.buffer : "<empty>",
              (unsigned long)input.buffer_len,
              input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
      fflush(stderr);
    }

    if (pal_maybe_buffer_rewrite_event(input)) {
      return true;
    }

    bool consumed = PAL_CALLBACK(input) != 0;
    if (consumed && input.status == PAL_INPUT_STATUS_PRESSED) {
      pal_remember_swallowed_keyup(input.key_code);
    } else if (!consumed && input.status == PAL_INPUT_STATUS_PRESSED) {
      pal_forget_swallowed_keyup(input.key_code);
    }
    return consumed;
  }

  input.event_type = PAL_INPUT_EVENT_MOUSE;
  input.status = PAL_INPUT_STATUS_PRESSED;
  input.key_code = 0;
  copy_current_input_source_fingerprint(&input);
  pal_mark_rewrite_active(&input);

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] cgevent mouse type=%u source=%s\n",
            type,
            input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
    fflush(stderr);
  }

  return PAL_CALLBACK(input) != 0;
}

static CGEventRef event_tap_callback(
    CGEventTapProxy proxy,
    CGEventType type,
    CGEventRef event,
    void *refcon) {
  (void)proxy;
  (void)refcon;

  if (type == kCGEventTapDisabledByTimeout || type == kCGEventTapDisabledByUserInput) {
    if (PAL_EVENT_TAP != nullptr) {
      CGEventTapEnable(PAL_EVENT_TAP, true);
      fprintf(stderr, "[rime-poc native] re-enabled CGEvent tap after disable event type=%u\n", type);
      fflush(stderr);
    }
    return event;
  }

  bool consumed = dispatch_cg_event(type, event);
  return consumed ? nullptr : event;
}

static NSTextField *pal_make_label(NSString *text, NSFont *font, NSColor *color) {
  NSTextField *label = [NSTextField labelWithString:text != nil ? text : @""];
  label.font = font;
  label.textColor = color;
  label.lineBreakMode = NSLineBreakByTruncatingTail;
  label.maximumNumberOfLines = 1;
  label.translatesAutoresizingMaskIntoConstraints = YES;
  [label sizeToFit];
  return label;
}

static NSArray<NSString *> *pal_split_candidates(NSString *candidates) {
  if (candidates == nil || candidates.length == 0) {
    return @[];
  }

  NSArray<NSString *> *parts = [candidates componentsSeparatedByString:@"\n"];
  NSMutableArray<NSString *> *result = [NSMutableArray arrayWithCapacity:parts.count];
  for (NSString *part in parts) {
    if (part.length > 0) {
      [result addObject:part];
    }
  }
  return result;
}

static NSScreen *pal_screen_for_point(NSPoint point) {
  for (NSScreen *screen in [NSScreen screens]) {
    if (NSPointInRect(point, screen.visibleFrame)) {
      return screen;
    }
  }
  return [NSScreen mainScreen];
}

typedef enum {
  PAL_CANDIDATE_ANCHOR_CARET = 0,
  PAL_CANDIDATE_ANCHOR_FOCUSED_ELEMENT = 1,
  PAL_CANDIDATE_ANCHOR_MOUSE = 2,
} PalCandidateAnchorKind;

typedef struct {
  NSPoint point;
  PalCandidateAnchorKind kind;
} PalCandidateAnchor;

static bool pal_ax_rect_is_usable(CGRect rect) {
  return isfinite(rect.origin.x) &&
      isfinite(rect.origin.y) &&
      isfinite(rect.size.width) &&
      isfinite(rect.size.height) &&
      rect.size.width >= 0.0 &&
      rect.size.height > 0.0;
}

static CGFloat pal_global_max_screen_y() {
  CGFloat max_y = 0.0;
  BOOL has_screen = NO;
  for (NSScreen *screen in [NSScreen screens]) {
    max_y = has_screen ? MAX(max_y, NSMaxY(screen.frame)) : NSMaxY(screen.frame);
    has_screen = YES;
  }
  return has_screen ? max_y : 0.0;
}

static NSRect pal_appkit_rect_from_ax_rect(CGRect ax_rect) {
  CGFloat global_max_y = pal_global_max_screen_y();
  return NSMakeRect(
      ax_rect.origin.x,
      global_max_y - ax_rect.origin.y - ax_rect.size.height,
      ax_rect.size.width,
      ax_rect.size.height);
}

static bool pal_rect_looks_like_text_target(NSRect rect) {
  if (rect.size.width <= 0.0 || rect.size.height <= 0.0) {
    return false;
  }

  NSScreen *screen = pal_screen_for_point(NSMakePoint(NSMidX(rect), NSMidY(rect)));
  NSRect visible = screen != nil ? screen.visibleFrame : NSMakeRect(0, 0, 800, 600);
  if (rect.size.height > 180.0) {
    return false;
  }
  if (rect.size.width > visible.size.width * 0.95 &&
      rect.size.height > visible.size.height * 0.35) {
    return false;
  }
  return true;
}

static AXUIElementRef pal_copy_focused_element() {
  if (!AXIsProcessTrusted()) {
    return nullptr;
  }

  AXUIElementRef system = AXUIElementCreateSystemWide();
  if (system == nullptr) {
    return nullptr;
  }

  CFTypeRef focused_ref = nullptr;
  AXError error = AXUIElementCopyAttributeValue(
      system,
      kAXFocusedUIElementAttribute,
      &focused_ref);
  CFRelease(system);

  if (error != kAXErrorSuccess || focused_ref == nullptr) {
    return nullptr;
  }
  if (CFGetTypeID(focused_ref) != AXUIElementGetTypeID()) {
    CFRelease(focused_ref);
    return nullptr;
  }
  return (AXUIElementRef)focused_ref;
}

static bool pal_copy_ax_position_and_size(AXUIElementRef element, CGRect *rect) {
  if (element == nullptr || rect == nullptr) {
    return false;
  }

  CFTypeRef position_ref = nullptr;
  CFTypeRef size_ref = nullptr;
  CGPoint position = CGPointZero;
  CGSize size = CGSizeZero;
  bool ok = false;

  AXError position_error = AXUIElementCopyAttributeValue(
      element,
      kAXPositionAttribute,
      &position_ref);
  AXError size_error = AXUIElementCopyAttributeValue(
      element,
      kAXSizeAttribute,
      &size_ref);

  if (position_error == kAXErrorSuccess &&
      size_error == kAXErrorSuccess &&
      position_ref != nullptr &&
      size_ref != nullptr &&
      CFGetTypeID(position_ref) == AXValueGetTypeID() &&
      CFGetTypeID(size_ref) == AXValueGetTypeID() &&
      AXValueGetType((AXValueRef)position_ref) == kAXValueCGPointType &&
      AXValueGetType((AXValueRef)size_ref) == kAXValueCGSizeType &&
      AXValueGetValue(
          (AXValueRef)position_ref,
          (AXValueType)kAXValueCGPointType,
          &position) &&
      AXValueGetValue(
          (AXValueRef)size_ref,
          (AXValueType)kAXValueCGSizeType,
          &size)) {
    CGRect candidate = CGRectMake(position.x, position.y, size.width, size.height);
    if (pal_ax_rect_is_usable(candidate)) {
      *rect = candidate;
      ok = true;
    }
  }

  if (position_ref != nullptr) {
    CFRelease(position_ref);
  }
  if (size_ref != nullptr) {
    CFRelease(size_ref);
  }
  return ok;
}

static bool pal_copy_parameterized_rect(
    AXUIElementRef element,
    CFStringRef attribute,
    CFTypeRef parameter,
    CGRect *rect) {
  if (element == nullptr || attribute == nullptr || parameter == nullptr || rect == nullptr) {
    return false;
  }

  CFTypeRef bounds_ref = nullptr;
  bool ok = false;

  AXError bounds_error = AXUIElementCopyParameterizedAttributeValue(
      element,
      attribute,
      parameter,
      &bounds_ref);
  if (bounds_error == kAXErrorSuccess &&
      bounds_ref != nullptr &&
      CFGetTypeID(bounds_ref) == AXValueGetTypeID() &&
      AXValueGetType((AXValueRef)bounds_ref) == kAXValueCGRectType) {
    CGRect bounds = CGRectZero;
    if (AXValueGetValue(
            (AXValueRef)bounds_ref,
            (AXValueType)kAXValueCGRectType,
            &bounds) &&
        pal_ax_rect_is_usable(bounds)) {
      *rect = bounds;
      ok = true;
    }
  }

  if (bounds_ref != nullptr) {
    CFRelease(bounds_ref);
  }
  return ok;
}

static bool pal_copy_selected_text_range_bounds(AXUIElementRef focused, CGRect *rect) {
  CFTypeRef selected_range_ref = nullptr;
  bool ok = false;

  AXError range_error = AXUIElementCopyAttributeValue(
      focused,
      kAXSelectedTextRangeAttribute,
      &selected_range_ref);
  if (range_error == kAXErrorSuccess &&
      selected_range_ref != nullptr &&
      CFGetTypeID(selected_range_ref) == AXValueGetTypeID() &&
      AXValueGetType((AXValueRef)selected_range_ref) == kAXValueCFRangeType) {
    ok = pal_copy_parameterized_rect(
        focused,
        kAXBoundsForRangeParameterizedAttribute,
        selected_range_ref,
        rect);
  }

  if (selected_range_ref != nullptr) {
    CFRelease(selected_range_ref);
  }
  return ok;
}

static bool pal_copy_selected_text_marker_bounds(AXUIElementRef focused, CGRect *rect) {
  CFTypeRef marker_range_ref = nullptr;
  bool ok = false;

  AXError marker_error = AXUIElementCopyAttributeValue(
      focused,
      CFSTR("AXSelectedTextMarkerRange"),
      &marker_range_ref);
  if (marker_error == kAXErrorSuccess && marker_range_ref != nullptr) {
    ok = pal_copy_parameterized_rect(
        focused,
        CFSTR("AXBoundsForTextMarkerRange"),
        marker_range_ref,
        rect);
  }

  if (marker_range_ref != nullptr) {
    CFRelease(marker_range_ref);
  }
  return ok;
}

static bool pal_copy_caret_bounds(CGRect *rect) {
  AXUIElementRef focused = pal_copy_focused_element();
  if (focused == nullptr) {
    return false;
  }

  bool ok = pal_copy_selected_text_range_bounds(focused, rect) ||
      pal_copy_selected_text_marker_bounds(focused, rect);
  CFRelease(focused);
  return ok;
}

static bool pal_copy_focused_element_bounds(CGRect *rect) {
  AXUIElementRef focused = pal_copy_focused_element();
  if (focused == nullptr) {
    return false;
  }

  bool ok = pal_copy_ax_position_and_size(focused, rect);
  CFRelease(focused);
  return ok;
}

static PalCandidateAnchor pal_candidate_anchor() {
  CGRect ax_rect = CGRectZero;

  if (pal_copy_caret_bounds(&ax_rect)) {
    NSRect rect = pal_appkit_rect_from_ax_rect(ax_rect);
    return { NSMakePoint(NSMinX(rect), NSMinY(rect)), PAL_CANDIDATE_ANCHOR_CARET };
  }

  if (pal_copy_focused_element_bounds(&ax_rect)) {
    NSRect rect = pal_appkit_rect_from_ax_rect(ax_rect);
    if (pal_rect_looks_like_text_target(rect)) {
      return {
        NSMakePoint(NSMinX(rect) + 10.0, NSMinY(rect)),
        PAL_CANDIDATE_ANCHOR_FOCUSED_ELEMENT
      };
    }
  }

  return { [NSEvent mouseLocation], PAL_CANDIDATE_ANCHOR_MOUSE };
}

static NSRect pal_candidate_panel_frame(NSSize size, PalCandidateAnchor anchor) {
  NSScreen *screen = pal_screen_for_point(anchor.point);
  NSRect visible = screen != nil ? screen.visibleFrame : NSMakeRect(0, 0, 800, 600);
  CGFloat margin = 12.0;
  CGFloat horizontal_offset = anchor.kind == PAL_CANDIDATE_ANCHOR_MOUSE ? 18.0 : 0.0;
  CGFloat vertical_offset = anchor.kind == PAL_CANDIDATE_ANCHOR_MOUSE ? 18.0 : 8.0;
  CGFloat x = anchor.point.x + horizontal_offset;
  CGFloat y = anchor.point.y - size.height - vertical_offset;

  if (x + size.width > NSMaxX(visible) - margin) {
    x = NSMaxX(visible) - size.width - margin;
  }
  if (x < NSMinX(visible) + margin) {
    x = NSMinX(visible) + margin;
  }

  // Text anchors must stay below the insertion point. Only the mouse fallback
  // may flip or clamp vertically to remain visible.
  if (anchor.kind == PAL_CANDIDATE_ANCHOR_MOUSE) {
    if (y < NSMinY(visible) + margin) {
      y = anchor.point.y + vertical_offset;
    }
    if (y + size.height > NSMaxY(visible) - margin) {
      y = NSMaxY(visible) - size.height - margin;
    }
    if (y < NSMinY(visible) + margin) {
      y = NSMinY(visible) + margin;
    }
  }

  return NSMakeRect(round(x), round(y), round(size.width), round(size.height));
}

static void pal_ensure_candidate_panel() {
  if (PAL_CANDIDATE_PANEL != nil) {
    return;
  }

  NSRect initial_frame = NSMakeRect(0, 0, 260, 74);
  PAL_CANDIDATE_PANEL = [[NSPanel alloc]
      initWithContentRect:initial_frame
                styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
                  backing:NSBackingStoreBuffered
                    defer:NO];
  PAL_CANDIDATE_PANEL.releasedWhenClosed = NO;
  PAL_CANDIDATE_PANEL.opaque = NO;
  PAL_CANDIDATE_PANEL.backgroundColor = [NSColor clearColor];
  PAL_CANDIDATE_PANEL.hasShadow = YES;
  PAL_CANDIDATE_PANEL.hidesOnDeactivate = NO;
  PAL_CANDIDATE_PANEL.ignoresMouseEvents = YES;
  PAL_CANDIDATE_PANEL.level = NSPopUpMenuWindowLevel;
  PAL_CANDIDATE_PANEL.collectionBehavior =
      NSWindowCollectionBehaviorCanJoinAllSpaces |
      NSWindowCollectionBehaviorFullScreenAuxiliary |
      NSWindowCollectionBehaviorTransient;

  PAL_CANDIDATE_CONTENT = [[NSView alloc] initWithFrame:initial_frame];
  PAL_CANDIDATE_CONTENT.wantsLayer = YES;
  PAL_CANDIDATE_CONTENT.layer.cornerRadius = 8.0;
  PAL_CANDIDATE_CONTENT.layer.masksToBounds = YES;
  PAL_CANDIDATE_CONTENT.layer.borderWidth = 1.0;
  PAL_CANDIDATE_CONTENT.layer.borderColor =
      [[NSColor separatorColor] colorWithAlphaComponent:0.55].CGColor;
  PAL_CANDIDATE_CONTENT.layer.backgroundColor =
      [[NSColor windowBackgroundColor] colorWithAlphaComponent:0.96].CGColor;
  PAL_CANDIDATE_PANEL.contentView = PAL_CANDIDATE_CONTENT;
}

static void pal_render_candidate_panel(
    NSString *preedit,
    NSArray<NSString *> *candidates,
    int32_t layout) {
  pal_ensure_candidate_panel();

  NSArray *subviews = [PAL_CANDIDATE_CONTENT.subviews copy];
  for (NSView *subview in subviews) {
    [subview removeFromSuperview];
  }
  [subviews release];

  CGFloat padding = 12.0;
  CGFloat row_gap = 7.0;
  CGFloat item_gap = 10.0;
  CGFloat min_width = 220.0;
  CGFloat item_height = 22.0;
  PalCandidateAnchor anchor = pal_candidate_anchor();
  NSScreen *screen = pal_screen_for_point(anchor.point);
  CGFloat max_width = (screen != nil ? screen.visibleFrame.size.width : 800.0) - 40.0;
  if (max_width < min_width) {
    max_width = min_width;
  }

  NSString *preedit_text = preedit.length > 0 ? preedit : @"Rime active";
  NSTextField *preedit_label = pal_make_label(
      preedit_text,
      [NSFont monospacedSystemFontOfSize:13.0 weight:NSFontWeightSemibold],
      [NSColor labelColor]);
  CGFloat content_max_width = max_width - padding * 2.0;
  NSSize preedit_size = preedit_label.fittingSize;
  CGFloat preedit_width = MIN(MAX(preedit_size.width, 80.0), content_max_width);

  NSMutableArray<NSTextField *> *candidate_labels = [NSMutableArray array];
  for (NSString *candidate in candidates) {
    NSTextField *label = pal_make_label(
        candidate,
        [NSFont systemFontOfSize:14.0 weight:NSFontWeightRegular],
        [NSColor labelColor]);
    [candidate_labels addObject:label];
  }

  if (candidate_labels.count == 0) {
    NSTextField *label = pal_make_label(
        @"Listening",
        [NSFont systemFontOfSize:13.0 weight:NSFontWeightRegular],
        [NSColor secondaryLabelColor]);
    [candidate_labels addObject:label];
  }

  BOOL vertical = layout == 1;
  CGFloat panel_width = min_width;
  CGFloat panel_height = 0.0;
  NSMutableArray<NSNumber *> *item_widths = [NSMutableArray arrayWithCapacity:candidate_labels.count];

  if (vertical) {
    CGFloat widest = preedit_width;
    for (NSTextField *label in candidate_labels) {
      CGFloat width = MIN(MAX(label.fittingSize.width, 90.0), content_max_width);
      [item_widths addObject:@(width)];
      widest = MAX(widest, width);
    }
    panel_width = MIN(MAX(widest + padding * 2.0, min_width), max_width);
    panel_height = padding + preedit_label.fittingSize.height + row_gap +
        candidate_labels.count * item_height +
        (candidate_labels.count - 1) * 3.0 + padding;
  } else {
    CGFloat row_width = 0.0;
    for (NSTextField *label in candidate_labels) {
      CGFloat width = MIN(MAX(label.fittingSize.width, 54.0), 180.0);
      [item_widths addObject:@(width)];
      row_width += width;
    }
    row_width += item_gap * (candidate_labels.count - 1);
    if (row_width > content_max_width && candidate_labels.count > 0) {
      [item_widths removeAllObjects];
      CGFloat equal_width =
          floor((content_max_width - item_gap * (candidate_labels.count - 1)) /
                candidate_labels.count);
      row_width = 0.0;
      for (NSUInteger i = 0; i < candidate_labels.count; i++) {
        CGFloat width = MAX(equal_width, 1.0);
        [item_widths addObject:@(width)];
        row_width += width;
      }
      row_width += item_gap * (candidate_labels.count - 1);
    }
    panel_width = MIN(MAX(MAX(preedit_width, row_width) + padding * 2.0, min_width), max_width);
    panel_height = padding + preedit_label.fittingSize.height + row_gap + item_height + padding;
  }

  NSSize panel_size = NSMakeSize(panel_width, panel_height);
  [PAL_CANDIDATE_PANEL setFrame:pal_candidate_panel_frame(panel_size, anchor) display:NO];
  PAL_CANDIDATE_CONTENT.frame = NSMakeRect(0, 0, panel_width, panel_height);

  CGFloat y = panel_height - padding - preedit_label.fittingSize.height;
  preedit_label.frame = NSMakeRect(
      padding,
      y,
      panel_width - padding * 2.0,
      preedit_label.fittingSize.height);
  [PAL_CANDIDATE_CONTENT addSubview:preedit_label];

  y -= row_gap + item_height;
  CGFloat x = padding;
  for (NSUInteger i = 0; i < candidate_labels.count; i++) {
    NSTextField *label = candidate_labels[i];
    CGFloat width = item_widths[i].doubleValue;
    if (vertical) {
      label.frame = NSMakeRect(padding, y, panel_width - padding * 2.0, item_height);
      y -= item_height + 3.0;
    } else {
      label.frame = NSMakeRect(x, y, width, item_height);
      x += width + item_gap;
    }
    [PAL_CANDIDATE_CONTENT addSubview:label];
  }

  [PAL_CANDIDATE_PANEL orderFrontRegardless];
}

extern "C" void pal_pinyin_update_candidate_panel(
    const char *preedit,
    const char *candidates,
    int32_t layout) {
  char *preedit_copy = strdup(preedit != nullptr ? preedit : "");
  char *candidates_copy = strdup(candidates != nullptr ? candidates : "");

  dispatch_async(dispatch_get_main_queue(), ^{
    @autoreleasepool {
      NSString *preedit_string = [NSString stringWithUTF8String:preedit_copy];
      NSString *candidates_string = [NSString stringWithUTF8String:candidates_copy];
      free(preedit_copy);
      free(candidates_copy);

      if (preedit_string == nil) {
        preedit_string = @"";
      }
      if (candidates_string == nil) {
        candidates_string = @"";
      }
      pal_render_candidate_panel(preedit_string, pal_split_candidates(candidates_string), layout);
    }
  });
}

extern "C" void pal_pinyin_hide_candidate_panel() {
  dispatch_async(dispatch_get_main_queue(), ^{
    @autoreleasepool {
      if (PAL_CANDIDATE_PANEL != nil) {
        [PAL_CANDIDATE_PANEL orderOut:nil];
      }
    }
  });
}

extern "C" void pal_pinyin_begin_rewrite_transaction() {
  if (PAL_REWRITE_TRANSACTION_ACTIVE) {
    if (PAL_LOG_EVENTS) {
      fprintf(stderr,
              "[rime-poc native] rewrite transaction already active generation=%llu buffered=%lu\n",
              (unsigned long long)PAL_REWRITE_TRANSACTION_GENERATION,
              (unsigned long)PAL_REWRITE_BUFFERED_EVENTS.size());
      fflush(stderr);
    }
    return;
  }

  PAL_REWRITE_TRANSACTION_ACTIVE = true;
  PAL_REWRITE_TRANSACTION_GENERATION += 1;
  PAL_REWRITE_BUFFERED_EVENTS.clear();
  PAL_REWRITE_SWALLOWED_KEYUPS.clear();
  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] rewrite transaction begin generation=%llu\n",
            (unsigned long long)PAL_REWRITE_TRANSACTION_GENERATION);
    fflush(stderr);
  }
}

extern "C" void pal_pinyin_finish_rewrite_transaction_after_delay(int32_t delay_ms) {
  int64_t clamped_delay_ms = delay_ms < 0 ? 0 : delay_ms;
  uint64_t generation = PAL_REWRITE_TRANSACTION_GENERATION;
  dispatch_after(
      dispatch_time(DISPATCH_TIME_NOW, clamped_delay_ms * NSEC_PER_MSEC),
      dispatch_get_main_queue(),
      ^{
        @autoreleasepool {
          if (!PAL_REWRITE_TRANSACTION_ACTIVE ||
              generation != PAL_REWRITE_TRANSACTION_GENERATION) {
            return;
          }

          PAL_REWRITE_TRANSACTION_ACTIVE = false;
          PAL_REWRITE_SWALLOWED_KEYUPS.clear();
          std::vector<PalInputEvent> events = PAL_REWRITE_BUFFERED_EVENTS;
          PAL_REWRITE_BUFFERED_EVENTS.clear();
          if (PAL_LOG_EVENTS) {
            fprintf(stderr,
                    "[rime-poc native] rewrite transaction finish generation=%llu buffered=%lu\n",
                    (unsigned long long)generation,
                    (unsigned long)events.size());
            fflush(stderr);
          }
          pal_replay_rewrite_buffered_events(events);
        }
      });
}

extern "C" void pal_pinyin_abort_rewrite_transaction() {
  if (PAL_REWRITE_OPERATION_RUNNING || !PAL_REWRITE_OPERATION_QUEUE.empty()) {
    fprintf(stderr,
            "[rime-poc native] refused to abort committed rewrite transaction running=%d queued=%lu\n",
            PAL_REWRITE_OPERATION_RUNNING ? 1 : 0,
            (unsigned long)PAL_REWRITE_OPERATION_QUEUE.size());
    fflush(stderr);
    return;
  }
  pal_pinyin_finish_rewrite_transaction_after_delay(0);
}

extern "C" void pal_pinyin_commit_rewrite_transaction(
    uint32_t delete_count,
    const char *replacement_text,
    int32_t delay_ms) {
  std::string replacement = replacement_text == nullptr ? "" : replacement_text;
  int32_t clamped_delay_ms = delay_ms < 0 ? 0 : delay_ms;
  dispatch_async(dispatch_get_main_queue(), ^{
    @autoreleasepool {
      PAL_REWRITE_OPERATION_QUEUE.push_back(
          PalRewriteOperation{delete_count, replacement, clamped_delay_ms});
      if (PAL_LOG_EVENTS) {
        fprintf(stderr,
                "[rime-poc native] rewrite operation queued delete=%u replacement_len=%lu queued=%lu active=%d running=%d\n",
                delete_count,
                (unsigned long)replacement.size(),
                (unsigned long)PAL_REWRITE_OPERATION_QUEUE.size(),
                PAL_REWRITE_TRANSACTION_ACTIVE ? 1 : 0,
                PAL_REWRITE_OPERATION_RUNNING ? 1 : 0);
        fflush(stderr);
      }
      pal_start_next_rewrite_operation();
    }
  });
}

extern "C" void pal_pinyin_start_event_loop(PalEventCallback callback) {
  PAL_CALLBACK = callback;

  @autoreleasepool {
    PAL_LOG_EVENTS = native_event_logging_enabled();
    fprintf(stderr,
            "[rime-poc native] starting Cocoa event loop pid=%d log_events=%s\n",
            getpid(),
            PAL_LOG_EVENTS ? "true" : "false");
    fflush(stderr);

    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    pal_refresh_keyboard_layout();

    PAL_WORKSPACE_OBSERVER = [[[NSWorkspace sharedWorkspace] notificationCenter]
        addObserverForName:NSWorkspaceDidActivateApplicationNotification
                    object:nil
                     queue:[NSOperationQueue mainQueue]
                usingBlock:^(NSNotification *notification) {
                  (void)notification;
                  dispatch_context_event("active_application_changed");
                }];

    PAL_INPUT_SOURCE_OBSERVER = [[NSNotificationCenter defaultCenter]
        addObserverForName:NSTextInputContextKeyboardSelectionDidChangeNotification
                    object:nil
                     queue:[NSOperationQueue mainQueue]
                usingBlock:^(NSNotification *notification) {
                  (void)notification;
                  pal_refresh_keyboard_layout();
                  dispatch_context_event("keyboard_selection_changed");
                }];

    CGEventMask tap_mask =
        CGEventMaskBit(kCGEventKeyDown) |
        CGEventMaskBit(kCGEventKeyUp) |
        CGEventMaskBit(kCGEventFlagsChanged) |
        CGEventMaskBit(kCGEventLeftMouseDown) |
        CGEventMaskBit(kCGEventRightMouseDown) |
        CGEventMaskBit(kCGEventOtherMouseDown);

    PAL_EVENT_TAP = CGEventTapCreate(
        kCGSessionEventTap,
        kCGHeadInsertEventTap,
        kCGEventTapOptionDefault,
        tap_mask,
        event_tap_callback,
        nullptr);

    if (PAL_EVENT_TAP != nullptr) {
      PAL_EVENT_TAP_SOURCE = CFMachPortCreateRunLoopSource(
          kCFAllocatorDefault,
          PAL_EVENT_TAP,
          0);
      CFRunLoopAddSource(
          CFRunLoopGetCurrent(),
          PAL_EVENT_TAP_SOURCE,
          kCFRunLoopCommonModes);
      CGEventTapEnable(PAL_EVENT_TAP, true);
      fprintf(stderr, "[rime-poc native] CGEvent tap registered\n");
      fflush(stderr);
      CFRunLoopRun();
      return;
    }

    fprintf(stderr,
            "[rime-poc native] failed to register required suppressing CGEvent tap; listener cannot start safely\n");
    fflush(stderr);
    PAL_CALLBACK = nullptr;
  }
}

extern "C" void pal_pinyin_inject_backspaces(uint32_t count, int32_t delay_ms) {
  fprintf(stderr,
          "[rime-poc native] injecting backspaces count=%u delay_ms=%d\n",
          count,
          delay_ms);
  fflush(stderr);
  dispatch_async(dispatch_get_main_queue(), ^{
    @autoreleasepool {
      for (uint32_t i = 0; i < count; i++) {
        post_key(0x33, true, delay_ms);
        post_key(0x33, false, delay_ms);
      }
    }
  });
}

extern "C" void pal_pinyin_inject_string(const char *string, int32_t delay_ms) {
  char *string_copy = strdup(string);
  fprintf(stderr, "[rime-poc native] injecting unicode text delay_ms=%d\n", delay_ms);
  fflush(stderr);

  dispatch_async(dispatch_get_main_queue(), ^{
    @autoreleasepool {
      NSString *ns_string = [NSString stringWithUTF8String:string_copy];
      free(string_copy);

      if (ns_string == nil) {
        return;
      }

      CFStringRef cf_string = (__bridge CFStringRef)ns_string;
      std::vector<UniChar> buffer(ns_string.length);
      CFStringGetCharacters(cf_string, CFRangeMake(0, ns_string.length), buffer.data());

      size_t index = 0;
      while (index < buffer.size()) {
        size_t chunk_size = 20;
        if (index + chunk_size > buffer.size()) {
          chunk_size = buffer.size() - index;
        }

        CGEventRef keydown = CGEventCreateKeyboardEvent(NULL, 0x31, true);
        CGEventSetLocation(keydown, CGPointMake(PAL_EVENT_MARKER, 0));
        CGEventKeyboardSetUnicodeString(keydown, (UniCharCount)chunk_size, buffer.data() + index);
        CGEventPost(kCGHIDEventTap, keydown);
        CFRelease(keydown);
        usleep(delay_ms * 1000);

        CGEventRef keyup = CGEventCreateKeyboardEvent(NULL, 0x31, false);
        CGEventSetLocation(keyup, CGPointMake(PAL_EVENT_MARKER, 0));
        CGEventPost(kCGHIDEventTap, keyup);
        CFRelease(keyup);
        usleep(delay_ms * 1000);

        index += chunk_size;
      }
    }
  });
}
