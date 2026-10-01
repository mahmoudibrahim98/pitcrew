// Input method editors (for Japanese, Chinese, Korean and others) use Enter to accept a candidate.
// That Enter must not send a message or submit a form.

/**
 * Whether a key press is part of an IME composition. Safari reports the Enter that ends a
 * composition with `isComposing` false, but with `keyCode` 229, as every browser does for keys
 * the IME handles.
 */
export function isComposing(event: { nativeEvent: { isComposing: boolean }; keyCode: number }): boolean {
  return event.nativeEvent.isComposing || event.keyCode === 229;
}
