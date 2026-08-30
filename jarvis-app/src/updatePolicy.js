export const AUTOMATIC_UPDATE_DELAY_MS = 2500;

/** Pure policy kept separate from the Tauri bridge so startup behavior is
 * deterministic and unit-testable.
 * @param {boolean} configured
 * @param {boolean} authenticated
 * @param {boolean} alreadyScheduled
 */
export function shouldScheduleAutomaticUpdateCheck(configured, authenticated, alreadyScheduled) {
  return configured && authenticated && !alreadyScheduled;
}
