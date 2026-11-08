// The oldest service this mod accepts, and how the service's version, as
// `GET /v1/service` tells it or `nowcast --version` prints it, compares to it
// (`00-cadrage.md` and `02-clients.md` on the design branch). Read without
// touching the engine.

/**
 * The oldest version of the service this mod accepts. The change that makes
 * the mod rely on something newer in the service raises it.
 */
export const OLDEST_SERVICE = '0.1.0-alpha.2'

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z.-]+)?$/

/** The version in `nowcast --version` output, `nowcast 0.1.0-alpha.2`; null if none. */
export function printedVersion(stdout: string): string | null {
  const match = /^nowcast (\S+)$/.exec(stdout.trim())
  return match?.[1] !== undefined && SEMVER.test(match[1]) ? match[1] : null
}

/** Whether `text` is a semver version, as `0.1.0-alpha.2`. */
export function isVersion(text: string): boolean {
  return SEMVER.test(text)
}

/** Whether `version` comes before `than` in semver precedence. */
export function isOlder(version: string, than: string): boolean {
  return compare(version, than) < 0
}

function compare(a: string, b: string): number {
  const [, ...x] = SEMVER.exec(a) ?? []
  const [, ...y] = SEMVER.exec(b) ?? []
  for (let i = 0; i < 3; i++) {
    const diff = Number(x[i]) - Number(y[i])
    if (diff !== 0) return diff
  }
  return comparePre(x[3], y[3])
}

/** A version without pre-release comes after one with it (semver 11.3). */
function comparePre(a: string | undefined, b: string | undefined): number {
  if (a === b) return 0
  if (a === undefined) return 1
  if (b === undefined) return -1
  const x = a.split('.')
  const y = b.split('.')
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const p = x[i] as string
    const q = y[i] as string
    if (p === q) continue
    const pn = /^\d+$/.test(p)
    const qn = /^\d+$/.test(q)
    if (pn && qn) return Number(p) - Number(q)
    // A numeric identifier comes before an alphanumeric one.
    if (pn !== qn) return pn ? -1 : 1
    return p < q ? -1 : 1
  }
  return x.length - y.length
}
