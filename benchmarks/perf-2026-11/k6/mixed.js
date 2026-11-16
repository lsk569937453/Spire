// k6 mixed-traffic scenario for the chaos experiment.
//
// Ten endpoints, fixed arrival rate. Exactly ~10% of the requests go to the
// endpoint chaosblade slows down (TARGET, default 7); the other 90% are spread
// over the remaining nine. One Trend per endpoint is exported because that is
// the whole point: does the seventh being slow drag down the other nine?
//
// Environment:
//   BASE      base URL of the gateway (e.g. http://spire:6667)
//   RATE      requests per second (default 1000)
//   DURATION  run duration (default 60s)
//   TARGET    the chaos-injected endpoint number (default 7)
//   VUS       pre-allocated VUs (default 400)
//   TAG       label used for the exported JSON summary
import http from 'k6/http';
import { Trend } from 'k6/metrics';
import { check } from 'k6';

const BASE = __ENV.BASE || 'http://spire:6667';
const TARGET = parseInt(__ENV.TARGET || '7', 10);
const RATE = parseInt(__ENV.RATE || '1000', 10);
const DURATION = __ENV.DURATION || '60s';
const TAG = __ENV.TAG || 'chaos';

// k6 does not keep tag sub-metrics in the JSON summary, so declare one Trend
// per endpoint explicitly.
const perEndpoint = {};
for (let i = 1; i <= 10; i += 1) {
  perEndpoint['api_' + i] = new Trend('api_' + i + '_ms', true);
}

export const options = {
  discardResponseBodies: true,
  scenarios: {
    mixed: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: parseInt(__ENV.VUS || '400', 10),
      maxVUs: parseInt(__ENV.VUS || '400', 10) * 4,
    },
  },
  thresholds: {},
};

export function pickEndpoint() {
  if (Math.random() < 0.1) {
    return TARGET;
  }
  let id = 1 + Math.floor(Math.random() * 10);
  if (id === TARGET) {
    id = (id % 10) + 1;
  }
  return id;
}

export default function () {
  const id = pickEndpoint();
  const res = http.get(BASE + '/api/' + id, {
    tags: { endpoint: 'api_' + id },
    timeout: '30s',
  });
  perEndpoint['api_' + id].add(res.timings.duration);
  check(res, { ok: (r) => r.status === 200 });
}

export function handleSummary(data) {
  const path = '/out/' + TAG + '.json';
  return {
    [path]: JSON.stringify(data, null, 2),
    stdout: 'summary written to ' + path + '\n',
  };
}
