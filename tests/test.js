// JavaScript Syntax Test
const serverConfig = { host: "127.0.0.1", port: 8080, active: true, auth: null };

function routeRequest(endpoint, retries) {
  if (!endpoint || retries <= 0) return false;
  const target = `http://${serverConfig.host}:${serverConfig.port}/${endpoint}`;
  return /api\/v1/.test(target);
}

const dispatch = (req) => routeRequest(req.path, 3);
export default dispatch;

