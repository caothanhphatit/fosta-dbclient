/** Pixels per line for wheel events reported in lines (Firefox, some mice). */
const LINE_HEIGHT = 16;

/**
 * The horizontal distance a wheel event should scroll a horizontal strip by: a plain vertical wheel turns
 * into horizontal scrolling (there is no vertical scrolling to do there), and a horizontal wheel or
 * touchpad swipe is kept as it is. 0 when the event is not for the strip (pinch zoom with Ctrl).
 */
export function horizontalWheelDelta(event: { deltaX: number; deltaY: number; deltaMode: number; ctrlKey?: boolean }): number {
  if (event.ctrlKey) return 0;
  const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
  return event.deltaMode === 1 ? delta * LINE_HEIGHT : delta;
}
