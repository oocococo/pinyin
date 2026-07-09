#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

typedef void (*PalEventCallback)(PalInputEvent event);

}

static constexpr CGFloat PAL_EVENT_MARKER = -27469;
static PalEventCallback PAL_CALLBACK = nullptr;
static id PAL_MONITOR = nil;
static id PAL_WORKSPACE_OBSERVER = nil;
static id PAL_INPUT_SOURCE_OBSERVER = nil;
static CFMachPortRef PAL_EVENT_TAP = nullptr;
static CFRunLoopSourceRef PAL_EVENT_TAP_SOURCE = nullptr;
static bool PAL_LOG_EVENTS = false;

enum {
  PAL_INPUT_MODIFIER_COMMAND = 1 << 0,
  PAL_INPUT_MODIFIER_CONTROL = 1 << 1,
  PAL_INPUT_MODIFIER_OPTION = 1 << 2,
  PAL_INPUT_MODIFIER_SHIFT = 1 << 3,
};

static bool native_event_logging_enabled() {
  const char *value = getenv("RIME_POC_NATIVE_LOG_EVENTS");
  if (value == nullptr) {
    return false;
  }
  return strcmp(value, "0") != 0 && strcmp(value, "false") != 0 && strcmp(value, "FALSE") != 0;
}

static bool is_marked_event(NSEvent *event) {
  return fabs(event.locationInWindow.x - PAL_EVENT_MARKER) < 0.001;
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

static uint32_t pal_modifier_flags_from_ns(NSEventModifierFlags flags) {
  uint32_t result = 0;
  if ((flags & NSEventModifierFlagCommand) != 0) {
    result |= PAL_INPUT_MODIFIER_COMMAND;
  }
  if ((flags & NSEventModifierFlagControl) != 0) {
    result |= PAL_INPUT_MODIFIER_CONTROL;
  }
  if ((flags & NSEventModifierFlagOption) != 0) {
    result |= PAL_INPUT_MODIFIER_OPTION;
  }
  if ((flags & NSEventModifierFlagShift) != 0) {
    result |= PAL_INPUT_MODIFIER_SHIFT;
  }
  return result;
}

static void copy_current_input_source_fingerprint(PalInputEvent *input);

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

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] context event reason=%s source=%s\n",
            reason,
            input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
    fflush(stderr);
  }

  PAL_CALLBACK(input);
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
  copy_string_to_input_buffer(input, string);
  CFRelease(string);
}

static void dispatch_cg_event(CGEventType type, CGEventRef event) {
  if (PAL_CALLBACK == nullptr) {
    return;
  }

  CGPoint location = CGEventGetLocation(event);
  if (fabs(location.x - PAL_EVENT_MARKER) < 0.001) {
    if (PAL_LOG_EVENTS) {
      fprintf(stderr, "[rime-poc native] skipped marked/self CGEvent type=%u\n", type);
      fflush(stderr);
    }
    return;
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

    PAL_CALLBACK(input);
    return;
  }

  input.event_type = PAL_INPUT_EVENT_MOUSE;
  input.status = PAL_INPUT_STATUS_PRESSED;
  input.key_code = 0;
  copy_current_input_source_fingerprint(&input);

  if (PAL_LOG_EVENTS) {
    fprintf(stderr,
            "[rime-poc native] cgevent mouse type=%u source=%s\n",
            type,
            input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
    fflush(stderr);
  }

  PAL_CALLBACK(input);
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

  dispatch_cg_event(type, event);
  return event;
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
        kCGEventTapOptionListenOnly,
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

    fprintf(stderr, "[rime-poc native] failed to register CGEvent tap; falling back to NSEvent monitor\n");
    fflush(stderr);

    NSEventMask mask = NSEventMaskKeyDown |
                       NSEventMaskKeyUp |
                       NSEventMaskFlagsChanged |
                       NSEventMaskLeftMouseDown |
                       NSEventMaskRightMouseDown |
                       NSEventMaskOtherMouseDown;

    PAL_MONITOR = [NSEvent addGlobalMonitorForEventsMatchingMask:mask handler:^(NSEvent *event) {
      @autoreleasepool {
        if (PAL_CALLBACK == nullptr || is_marked_event(event)) {
          if (PAL_LOG_EVENTS) {
            fprintf(stderr,
                    "[rime-poc native] skipped marked/self event type=%ld key=%hu\n",
                    (long)event.type,
                    event.keyCode);
            fflush(stderr);
          }
          return;
        }

        PalInputEvent input = {};
        if (event.type == NSEventTypeKeyDown ||
            event.type == NSEventTypeKeyUp ||
            event.type == NSEventTypeFlagsChanged) {
          input.event_type = PAL_INPUT_EVENT_KEYBOARD;
          input.status = event.type == NSEventTypeKeyUp
              ? PAL_INPUT_STATUS_RELEASED
              : PAL_INPUT_STATUS_PRESSED;
          input.key_code = event.keyCode;
          input.modifier_flags = pal_modifier_flags_from_ns(event.modifierFlags);
          if (event.type == NSEventTypeFlagsChanged &&
              (event.modifierFlags & NSEventModifierFlagShift) == 0) {
            input.status = PAL_INPUT_STATUS_RELEASED;
          }
          copy_current_input_source_fingerprint(&input);

          if (event.type != NSEventTypeFlagsChanged) {
            NSString *characters = event.characters;
            if (characters != nil) {
              const char *chars = [characters UTF8String];
              if (chars != nullptr) {
                strncpy(input.buffer, chars, sizeof(input.buffer) - 1);
                input.buffer[sizeof(input.buffer) - 1] = '\0';
                input.buffer_len = strlen(input.buffer);
              }
            }
          }

          if (PAL_LOG_EVENTS) {
            fprintf(stderr,
                    "[rime-poc native] key event type=%ld status=%d key=%u modifiers=%u chars=%s len=%lu source=%s\n",
                    (long)event.type,
                    input.status,
                    input.key_code,
                    input.modifier_flags,
                    input.buffer_len > 0 ? input.buffer : "<empty>",
                    (unsigned long)input.buffer_len,
                    input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
            fflush(stderr);
          }

          PAL_CALLBACK(input);
          return;
        }

        input.event_type = PAL_INPUT_EVENT_MOUSE;
        input.status = PAL_INPUT_STATUS_PRESSED;
        input.key_code = 0;
        copy_current_input_source_fingerprint(&input);
        if (PAL_LOG_EVENTS) {
          fprintf(stderr,
                  "[rime-poc native] mouse event type=%ld source=%s\n",
                  (long)event.type,
                  input.source_buffer_len > 0 ? input.source_buffer : "<unknown>");
          fflush(stderr);
        }
        PAL_CALLBACK(input);
      }
    }];

    if (PAL_MONITOR == nil) {
      fprintf(stderr, "[rime-poc native] failed to register global monitor\n");
    } else {
      fprintf(stderr, "[rime-poc native] global monitor registered\n");
    }
    fflush(stderr);

    [[NSRunLoop currentRunLoop] run];
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
