// Readiness probe: one request, non-zero exit if it is not a 200.
// Used because none of the benchmarked images ship a curl binary.
import http from 'k6/http';
import { check } from 'k6';

const URL = __ENV.URL;

export const options = {
  vus: 1,
  iterations: 1,
  thresholds: {
    checks: ['rate==1'],
  },
};

export default function () {
  const res = http.get(URL, { timeout: '5s' });
  check(res, { ready: (r) => r.status === 200 });
}
