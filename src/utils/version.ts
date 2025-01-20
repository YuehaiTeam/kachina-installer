export function version_compare(v1 = '', v2 = ''): number {
  let i: number;
  let x: number;
  let compare: number = 0;

  if (!v1 && !v2) {
    return 0;
  }
  if (!v1) {
    return -1;
  }
  if (!v2) {
    return 1;
  }

  const vm: { [key: string]: number } = {
    dev: -6,
    alpha: -5,
    a: -5,
    beta: -4,
    b: -4,
    RC: -3,
    rc: -3,
    '#': -2,
    p: 1,
    pl: 1,
  };

  const _prepVersion = function (v: string): (string | number)[] {
    v = ('' + v).replace(/[_\-+]/g, '.');
    v = v.replace(/([^.\d]+)/g, '.$1.').replace(/\.{2,}/g, '.');
    return !v.length ? [-8] : v.split('.');
  };

  const _numVersion = function (v: string | number): number {
    return !v ? 0 : isNaN(Number(v)) ? vm[v] || -7 : parseInt(v as string, 10);
  };

  const v1a = _prepVersion(v1);
  const v2a = _prepVersion(v2);
  x = Math.max(v1.length, v2.length);
  for (i = 0; i < x; i++) {
    if (v1a[i] === v2a[i]) {
      continue;
    }
    v1a[i] = _numVersion(v1[i]);
    v2a[i] = _numVersion(v2[i]);
    if (v1a[i] < v2a[i]) {
      compare = -1;
      break;
    } else if (v1a[i] > v2a[i]) {
      compare = 1;
      break;
    }
  }
  return compare;
}
