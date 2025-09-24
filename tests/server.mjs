import express from 'express';
import path from 'path';

const PORT = process.env.PORT || 8080;
const FIXTURES_DIR = './fixtures';

function createServer() {
  const app = express();

  // 启用Range请求支持
  app.use(express.static(path.resolve(FIXTURES_DIR), {
    acceptRanges: true,
    lastModified: true,
    etag: true
  }));

  // 日志中间件
  app.use((req, res, next) => {
    console.log(`${req.method} ${req.url}`);
    next();
  });

  return app;
}

async function startServer() {
  const app = createServer();

  return new Promise((resolve) => {
    const server = app.listen(PORT, () => {
      console.log(chalk.green(`Express server listening on port ${PORT}`));
      console.log(chalk.gray(`Serving files from: ${path.resolve(FIXTURES_DIR)}`));
      resolve(server);
    });

    // 优雅关闭
    process.on('SIGINT', () => {
      console.log('\\nShutting down server...');
      server.close(() => process.exit(0));
    });
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  startServer().catch(console.error);
}

export { startServer, createServer };