// Transcript scroll stickiness (design §3-4): stick-to-bottom within 40px,
// scroll-down FAB past 160px. Pure math so the thresholds are testable.
export const STICK_THRESHOLD_PX = 40;
export const FAB_THRESHOLD_PX = 160;

export function distanceFromBottom(scrollHeight: number, scrollTop: number, clientHeight: number): number {
  return Math.max(0, scrollHeight - scrollTop - clientHeight);
}

export function isStuck(distance: number): boolean {
  return distance < STICK_THRESHOLD_PX;
}

export function showScrollFab(distance: number): boolean {
  return distance > FAB_THRESHOLD_PX;
}
