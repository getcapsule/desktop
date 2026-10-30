const fs = require('node:fs');
const { execFileSync } = require('node:child_process');

function projectVersion() {
  const packageVersion = JSON.parse(fs.readFileSync('package.json', 'utf8')).version;
  const tauriVersion = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8')).version;
  const cargoVersion = fs.readFileSync('src-tauri/Cargo.toml', 'utf8').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!packageVersion || packageVersion !== tauriVersion || packageVersion !== cargoVersion) {
    throw new Error(`App versions disagree: package.json=${packageVersion}, tauri.conf.json=${tauriVersion}, Cargo.toml=${cargoVersion}`);
  }
  return packageVersion;
}

function versionFromTag(tag) {
  const match = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)(?:\.(0|[1-9]\d*))?$/.exec(tag);
  if (!match) throw new Error('Use a release tag such as v0.5.0.');
  return `${match[1]}.${match[2]}.${match[3] ?? '0'}`;
}

async function prepareRelease({ github, context, core }, options = {}) {
  const tag = options.tag ?? process.env.RELEASE_TAG;
  const version = options.version ?? projectVersion();
  if (versionFromTag(tag) !== version) {
    throw new Error(`Release tag ${tag} does not match app version ${version}. Update all version files before tagging.`);
  }
  const commit = options.commit ?? execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const releases = await github.paginate(github.rest.repos.listReleases, { ...context.repo, per_page: 100 });
  const matches = releases.filter(release => release.tag_name === tag);
  if (matches.length > 1) throw new Error(`Multiple releases use ${tag}; resolve the duplicate drafts first.`);
  if (matches[0] && !matches[0].draft) throw new Error(`${tag} is already published; use a new version tag.`);
  const release = matches[0] ?? (await github.rest.repos.createRelease({
    ...context.repo,
    tag_name: tag,
    target_commitish: commit,
    name: `capsule ${tag}`,
    draft: true,
    prerelease: false,
    body: '## Downloads\n\n- **Windows:** use the `.exe` installer. The build is unsigned.\n- **Linux:** use the `.deb` or `.AppImage` package.\n\nLocal files, Navidrome, public YouTube playback, and SoundCloud are supported. Acrylic and mica window materials are Windows-only.\n',
  })).data;
  core.setOutput('id', String(release.id));
  core.setOutput('tag', tag);
  core.setOutput('version', version);
  core.setOutput('source_sha', commit);
  await core.summary.addHeading('Release prepared').addLink('Draft release', release.html_url).write();
}

async function publishRelease({ github, context, core }, options = {}) {
  const id = Number(options.id ?? process.env.RELEASE_ID);
  const tag = options.tag ?? process.env.RELEASE_TAG;
  const version = options.version ?? process.env.RELEASE_VERSION;
  if (!Number.isSafeInteger(id) || id <= 0) throw new Error('Missing release ID from the preparation job.');
  if (versionFromTag(tag) !== version) throw new Error('Release tag and package version do not match.');
  const { data: release } = await github.rest.repos.getRelease({ ...context.repo, release_id: id });
  if (release.tag_name !== tag) throw new Error(`Release ID ${id} belongs to a different tag.`);
  const assets = await github.paginate(github.rest.repos.listReleaseAssets, { ...context.repo, release_id: id, per_page: 100 });
  const missing = ['.exe', '.deb', '.AppImage'].filter(extension => !assets.some(asset =>
    asset.name.endsWith(extension) && asset.name.includes(`_${version}_`) && asset.size > 0
  ));
  if (missing.length) {
    throw new Error(`Missing ${version} installers: ${missing.join(', ')}. Found: ${assets.map(asset => asset.name).join(', ') || 'no assets'}`);
  }
  const { data: published } = await github.rest.repos.updateRelease({
    ...context.repo, release_id: id, draft: false, prerelease: false, make_latest: 'true',
  });
  if (published.draft) throw new Error('GitHub still reports the release as a draft.');
  core.notice(`Published ${tag}: ${published.html_url}`);
  await core.summary.addHeading('Installers published').addLink('Published release', published.html_url).write();
}

module.exports = { prepareRelease, publishRelease, projectVersion };
