import express from 'express';
import fs from 'fs-extra';
import path from 'path';
import crypto from 'crypto';

const PORT = process.env.PORT || 8080;
const FIXTURES_DIR = './fixtures';
const EMBEDDED_MARK = Buffer.from('!IN\0');
const INDEX_MARK = Buffer.from('!KachinaInstaller!');

function versionFromResource(resource) {
  const name = path.posix.basename(resource);
  const match = name.match(/v(\d+)\.exe$/i);
  if (!match) {
    throw new Error(`Unsupported DFS2 resource: ${resource}`);
  }
  return `v${match[1]}`;
}

function readPackedEntries(data) {
  const entries = new Map();
  let offset = 0;
  while ((offset = data.indexOf(EMBEDDED_MARK, offset)) !== -1) {
    const nameLengthOffset = offset + EMBEDDED_MARK.length;
    const nameLength = data.readUInt16BE(nameLengthOffset);
    const nameOffset = nameLengthOffset + 2;
    const sizeOffset = nameOffset + nameLength;
    if (nameLength === 0 || nameLength > 512 || sizeOffset + 4 > data.length) {
      offset += EMBEDDED_MARK.length;
      continue;
    }
    const size = data.readUInt32BE(sizeOffset);
    const contentOffset = sizeOffset + 4;
    if (contentOffset + size > data.length) {
      offset += EMBEDDED_MARK.length;
      continue;
    }
    const name = data.subarray(nameOffset, sizeOffset).toString('utf8');
    entries.set(name, {
      name,
      offset: contentOffset,
      rawOffset: offset,
      size,
    });
    offset = contentOffset + size;
  }
  return entries;
}

function readInstallerEnd(data) {
  const mark = data.indexOf(INDEX_MARK);
  if (mark < 0 || mark + 38 > data.length) {
    throw new Error('Packed resource has no installer index header');
  }
  const indexStart = data.readUInt32BE(mark + INDEX_MARK.length);
  const configSize = data.readUInt32BE(mark + INDEX_MARK.length + 4);
  const themeSize = data.readUInt32BE(mark + INDEX_MARK.length + 8);
  return indexStart + configSize + themeSize;
}

function addIndexEntry(index, entries, hash, name) {
  const entry = entries.get(hash);
  if (!entry) {
    throw new Error(`Packed resource is missing embedded entry ${hash}`);
  }
  index[hash] = {
    name,
    offset: entry.offset,
    raw_offset: entry.rawOffset,
    size: entry.size,
  };
}

function loadResource(version) {
  const packagePath = path.resolve(FIXTURES_DIR, `test-app-${version}.exe`);
  const metadataPath = path.resolve(FIXTURES_DIR, `${version}-metadata.json`);
  const bytes = fs.readFileSync(packagePath);
  const metadata = JSON.parse(fs.readFileSync(metadataPath, 'utf8'));
  const entries = readPackedEntries(bytes);
  const index = {};

  for (const item of metadata.hashed ?? []) {
    if (item.xxh) {
      addIndexEntry(index, entries, item.xxh, item.file_name);
    }
    if (item.md5) {
      addIndexEntry(index, entries, item.md5, item.file_name);
    }
  }
  for (const patch of metadata.patches ?? []) {
    const from = patch.from?.xxh ?? patch.from?.md5;
    const to = patch.to?.xxh ?? patch.to?.md5;
    if (from && to) {
      addIndexEntry(index, entries, `${from}_${to}`, patch.file_name);
    }
  }

  return {
    bytes,
    metadata,
    data: {
      index,
      metadata,
      installer_end: readInstallerEnd(bytes),
    },
  };
}

function parseRange(value, length) {
  const match = /^(\d+)-(\d*)$/.exec(value ?? '');
  if (!match) {
    throw new Error(`Invalid DFS2 range: ${value}`);
  }
  const start = Number(match[1]);
  const requestedEnd = match[2] === '' ? length - 1 : Number(match[2]);
  const end = Math.min(requestedEnd, length - 1);
  if (start < 0 || start > end || end >= length) {
    throw new Error(`DFS2 range outside resource: ${value}`);
  }
  return { start, end };
}

function challengeData() {
  const source = `kachina-${crypto.randomBytes(8).toString('hex')}`;
  for (let i = 0; i <= 255; i += 1) {
    const suffix = i.toString(16).padStart(2, '0');
    const candidate = `${source}${suffix}`;
    const hash = crypto.createHash('md5').update(candidate).digest('hex');
    return { source, answer: candidate, data: `${hash}/${source}` };
  }
  throw new Error('Unable to generate DFS2 challenge');
}

function createState() {
  return {
    resources: new Map(),
    sessions: new Map(),
    batchRequests: [],
    singleRequests: [],
    downloadRequests: [],
    deleteRequests: [],
    httpRequests: [],
    faults: {
      httpAbortInjected: false,
    },
    getResource(version) {
      if (!this.resources.has(version)) {
        this.resources.set(version, loadResource(version));
      }
      return this.resources.get(version);
    },
  };
}

function resourceName(req) {
  return req.path.slice('/api/'.length);
}

function sessionPath(req) {
  const resource = req.path.startsWith('/api/')
    ? resourceName(req)
    : req.path.slice(1);
  const parts = resource.split('/').filter(Boolean);
  if (parts[0] !== 'session' || parts.length !== 3) {
    return null;
  }
  return { sid: parts[1], resource: parts[2] };
}

function signedUrl(req, version, sid, range) {
  const protocol = process.env.KACHINA_E2E_H3 === '1' ? 'http3' : req.protocol;
  return `${protocol}://${req.get('host')}/signed/${version}/${encodeURIComponent(sid)}?range=${encodeURIComponent(range)}`;
}

function createServer() {
  const app = express();
  const state = createState();

  app.use(express.json());
  app.use((req, _res, next) => {
    console.log(`${req.method} ${req.originalUrl}`);
    next();
  });

  app.get('/api/*', (req, res, next) => {
    try {
      if (sessionPath(req)) {
        return next();
      }
      const resource = resourceName(req);
      const version = versionFromResource(resource);
      const loaded = state.getResource(version);
      if (req.query.with_metadata !== '1') {
        return res.status(404).json({ error: 'metadata query is required' });
      }
      return res.json({
        resource_version: version,
        name: resource,
        data: loaded.data,
      });
    } catch (error) {
      return res.status(500).json({ error: error.message });
    }
  });

  app.post(['/api/*', '/session/:sid/:resource'], (req, res, next) => {
    try {
      const session = sessionPath(req);
      if (session) {
        const version = versionFromResource(session.resource);
        const ranges = req.body?.chunks ?? [];
        state.batchRequests.push({ ...session, ranges });
        const urls = Object.fromEntries(
          ranges.map((range) => [
            range,
            { url: signedUrl(req, version, session.sid, range) },
          ]),
        );
        return res.json({ urls });
      }

      const resource = resourceName(req);
      const version = versionFromResource(resource);
      const sid = req.body?.sid ?? `sid-${state.sessions.size + 1}`;
      const needsChallenge = req.query.challenge === '1';
      if (needsChallenge && !req.body?.challenge) {
        const challenge = challengeData();
        state.sessions.set(sid, {
          version,
          challenged: true,
          challenge: challenge.answer,
        });
        return res.status(402).json({
          sid,
          challenge: 'md5',
          data: challenge.data,
        });
      }
      if (needsChallenge) {
        const session = state.sessions.get(sid);
        if (!session || req.body?.challenge !== session.challenge) {
          return res.status(402).json({ error: 'invalid challenge response' });
        }
      }
      state.sessions.set(sid, { version, challenged: needsChallenge });
      return res.json({ sid });
    } catch (error) {
      return res.status(500).json({ error: error.message });
    }
  });

  app.get(
    ['/api/session/:sid/:resource', '/session/:sid/:resource'],
    (req, res) => {
      const version = versionFromResource(req.params.resource);
      const range = req.query.range;
      state.singleRequests.push({ sid: req.params.sid, version, range });
      return res.json({
        url: signedUrl(req, version, req.params.sid, range),
      });
    },
  );

  app.delete(
    ['/api/session/:sid/:resource', '/session/:sid/:resource'],
    (req, res) => {
      state.deleteRequests.push({
        sid: req.params.sid,
        resource: req.params.resource,
        body: req.body,
      });
      return res.json({ ok: true });
    },
  );

  app.get('/signed/:version/:sid', (req, res) => {
    try {
      const version = versionFromResource(`resource-${req.params.version}.exe`);
      const loaded = state.getResource(version);
      const range = parseRange(req.query.range, loaded.bytes.length);
      const bytes = loaded.bytes.subarray(range.start, range.end + 1);
      state.downloadRequests.push({
        sid: req.params.sid,
        version,
        range: `${range.start}-${range.end}`,
        path: req.path,
      });
      res.status(206);
      res.set(
        'Content-Range',
        `bytes ${range.start}-${range.end}/${loaded.bytes.length}`,
      );
      res.set('Accept-Ranges', 'bytes');
      return res.send(bytes);
    } catch (error) {
      return res.status(416).json({ error: error.message });
    }
  });

  app.get('/test-app-:version.exe', (req, res, next) => {
    try {
      const version = req.params.version.startsWith('v')
        ? req.params.version
        : `v${req.params.version}`;
      const loaded = state.getResource(version);
      const rangeHeader = req.headers.range;
      if (!rangeHeader) {
        return res.send(loaded.bytes);
      }
      const range = parseRange(
        rangeHeader.replace(/^bytes=/, ''),
        loaded.bytes.length,
      );
      const requestNumber =
        state.httpRequests.filter((request) => request.version === version)
          .length + 1;
      state.httpRequests.push({
        version,
        range: `${range.start}-${range.end}`,
        requestNumber,
      });
      res.status(206);
      res.set(
        'Content-Range',
        `bytes ${range.start}-${range.end}/${loaded.bytes.length}`,
      );
      res.set('Accept-Ranges', 'bytes');

      const abortAfter = Number(process.env.KACHINA_E2E_ABORT_AFTER ?? 0);
      const shouldAbort =
        process.env.KACHINA_E2E_ABORT_VERSION === version &&
        requestNumber > abortAfter;
      if (shouldAbort) {
        state.faults.httpAbortInjected = true;
        const length = range.end - range.start + 1;
        const partial = Math.max(1, Math.floor(length / 2));
        res.write(loaded.bytes.subarray(range.start, range.start + partial));
        return res.destroy();
      }
      return res.send(loaded.bytes.subarray(range.start, range.end + 1));
    } catch (error) {
      return next(error);
    }
  });

  app.use(
    express.static(path.resolve(FIXTURES_DIR), {
      acceptRanges: true,
      lastModified: true,
      etag: true,
    }),
  );

  app.use((req, res) => {
    res.status(404).json({ error: `not found: ${req.method} ${req.path}` });
  });

  return { app, state };
}

async function startServer() {
  const { app, state } = createServer();

  return new Promise((resolve, reject) => {
    const server = app.listen(PORT, () => {
      console.log(`Express server listening on port ${PORT}`);
      console.log(`Serving files from: ${path.resolve(FIXTURES_DIR)}`);
      server.dfs2State = state;
      resolve(server);
    });
    server.on('error', reject);
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  startServer().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}

export { startServer, createServer };
