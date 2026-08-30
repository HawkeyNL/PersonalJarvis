export const AUTOMATIC_UPDATE_DELAY_MS: number;
export function shouldScheduleAutomaticUpdateCheck(
  configured: boolean,
  authenticated: boolean,
  alreadyScheduled: boolean,
): boolean;
