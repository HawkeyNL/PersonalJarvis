export const AUTOMATIC_UPDATE_DELAY_MS: number;
export function shouldScheduleAutomaticUpdateCheck(
  authenticated: boolean,
  alreadyScheduled: boolean,
): boolean;
