#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
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

static constexpr CGFloat PAL_EVENT_MARKER = -27469;
static PalEventCallback PAL_CALLBACK = nullptr;
static id PAL_MONITOR = nil;
static CFMachPortRef PAL_EVENT_TAP = nullptr;
static CFRunLoopSourceRef PAL_EVENT_TAP_SOURCE = nullptr;
static bool PAL_LOG_EVENTS = false;

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

static void copy_string_to_input_buffer(PalInputEvent *input, CFStringRef string) {
  if (string == nullptr) {
    return;
  }

  if (CFStringGetCString(
          string,
          input->buffer,
          sizeof(input->buffer),
          kCFStringEncodingUTF8)) {
    input->buffer[sizeof(input->buffer) - 1] = '\0';
    input->buffer_len = strlen(input->buffer);
  }
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
  if (type == kCGEventKeyDown || type == kCGEventKeyUp) {
    input.event_type = PAL_INPUT_EVENT_KEYBOARD;
    input.status = type == kCGEventKeyDown
        ? PAL_INPUT_STATUS_PRESSED
        : PAL_INPUT_STATUS_RELEASED;
    input.key_code = (uint32_t)CGEventGetIntegerValueField(
        event,
        kCGKeyboardEventKeycode);
    copy_cg_event_text(&input, event);

    if (PAL_LOG_EVENTS) {
      fprintf(stderr,
              "[rime-poc native] cgevent key type=%u status=%d key=%u chars=%s len=%lu\n",
              type,
              input.status,
              input.key_code,
              input.buffer_len > 0 ? input.buffer : "<empty>",
              (unsigned long)input.buffer_len);
      fflush(stderr);
    }

    PAL_CALLBACK(input);
    return;
  }

  input.event_type = PAL_INPUT_EVENT_MOUSE;
  input.status = PAL_INPUT_STATUS_PRESSED;
  input.key_code = 0;

  if (PAL_LOG_EVENTS) {
    fprintf(stderr, "[rime-poc native] cgevent mouse type=%u\n", type);
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

    CGEventMask tap_mask =
        CGEventMaskBit(kCGEventKeyDown) |
        CGEventMaskBit(kCGEventKeyUp) |
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
        if (event.type == NSEventTypeKeyDown || event.type == NSEventTypeKeyUp) {
          input.event_type = PAL_INPUT_EVENT_KEYBOARD;
          input.status = event.type == NSEventTypeKeyDown
              ? PAL_INPUT_STATUS_PRESSED
              : PAL_INPUT_STATUS_RELEASED;
          input.key_code = event.keyCode;

          NSString *characters = event.characters;
          if (characters != nil) {
            const char *chars = [characters UTF8String];
            if (chars != nullptr) {
              strncpy(input.buffer, chars, sizeof(input.buffer) - 1);
              input.buffer[sizeof(input.buffer) - 1] = '\0';
              input.buffer_len = strlen(input.buffer);
            }
          }

          if (PAL_LOG_EVENTS) {
            fprintf(stderr,
                    "[rime-poc native] key event type=%ld status=%d key=%u chars=%s len=%lu\n",
                    (long)event.type,
                    input.status,
                    input.key_code,
                    input.buffer_len > 0 ? input.buffer : "<empty>",
                    (unsigned long)input.buffer_len);
            fflush(stderr);
          }

          PAL_CALLBACK(input);
          return;
        }

        input.event_type = PAL_INPUT_EVENT_MOUSE;
        input.status = PAL_INPUT_STATUS_PRESSED;
        input.key_code = 0;
        if (PAL_LOG_EVENTS) {
          fprintf(stderr, "[rime-poc native] mouse event type=%ld\n", (long)event.type);
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
