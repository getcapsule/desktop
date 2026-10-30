import { describe, expect, it, vi } from 'vitest'
import release from './release.cjs'

function boundary() {
  const draft = { id: 42, tag_name: 'v0.5.0', draft: true, html_url: 'https://github.com/getcapsule/desktop/releases/tag/v0.5.0' }
  const repos = {
    listReleases: vi.fn(),
    createRelease: vi.fn().mockResolvedValue({ data: draft }),
    getRelease: vi.fn().mockResolvedValue({ data: draft }),
    listReleaseAssets: vi.fn(),
    updateRelease: vi.fn().mockResolvedValue({ data: { ...draft, draft: false } }),
  }
  const state = {
    releases: [] as typeof draft[],
    assets: ['capsule_0.5.0_x64-setup.exe', 'capsule_0.5.0_amd64.deb', 'capsule_0.5.0_amd64.AppImage']
      .map(name => ({ name, size: 1024 })),
  }
  const github = {
    rest: { repos },
    paginate: vi.fn(async (route: unknown) => {
      if (route === repos.listReleases) return state.releases
      if (route === repos.listReleaseAssets) return state.assets
      throw new Error('Unexpected API call')
    }),
  }
  const core = {
    setOutput: vi.fn(), notice: vi.fn(),
    summary: { addHeading: vi.fn().mockReturnThis(), addLink: vi.fn().mockReturnThis(), write: vi.fn().mockResolvedValue(undefined) },
  }
  return { github, core, context: { repo: { owner: 'getcapsule', repo: 'desktop' } }, repos, state, draft }
}

const options = { tag: 'v0.5.0', version: '0.5.0', commit: 'a'.repeat(40) }

describe('release preparation', () => {
  it('creates one shared draft and exposes its ID and exact source commit to both builds', async () => {
    const api = boundary()
    await release.prepareRelease(api, options)
    expect(api.repos.createRelease).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ tag_name: 'v0.5.0', draft: true, target_commitish: options.commit }))
    expect(api.core.setOutput).toHaveBeenCalledWith('id', '42')
    expect(api.core.setOutput).toHaveBeenCalledWith('source_sha', options.commit)
  })

  it('reuses a matching draft instead of letting platform jobs create competing releases', async () => {
    const api = boundary()
    api.state.releases = [api.draft]
    await release.prepareRelease(api, options)
    expect(api.repos.createRelease).not.toHaveBeenCalled()
    expect(api.core.setOutput).toHaveBeenCalledWith('id', '42')
  })

  it('accepts a short version tag when it matches the full app version', async () => {
    const api = boundary()
    await release.prepareRelease(api, { ...options, tag: 'v0.5' })
    expect(api.repos.createRelease).toHaveBeenCalledWith(expect.objectContaining({ tag_name: 'v0.5' }))
  })

  it('rejects mismatched package versions before creating a release', async () => {
    const api = boundary()
    await expect(release.prepareRelease(api, { ...options, version: '0.3.0' })).rejects.toThrow(/version/i)
    expect(api.github.paginate).not.toHaveBeenCalled()
    expect(api.repos.createRelease).not.toHaveBeenCalled()
  })

  it('leaves already published releases untouched', async () => {
    const api = boundary()
    api.state.releases = [{ ...api.draft, draft: false }]
    await expect(release.prepareRelease(api, options)).rejects.toThrow(/already published/i)
    expect(api.repos.createRelease).not.toHaveBeenCalled()
    expect(api.repos.updateRelease).not.toHaveBeenCalled()
  })

  it('rejects ambiguous duplicate drafts', async () => {
    const api = boundary()
    api.state.releases = [api.draft, { ...api.draft, id: 43 }]
    await expect(release.prepareRelease(api, options)).rejects.toThrow(/multiple/i)
    expect(api.repos.createRelease).not.toHaveBeenCalled()
  })
})

describe('release publication', () => {
  it.each(['.exe', '.deb', '.AppImage'])('keeps the release private when the %s installer is missing', async extension => {
    const api = boundary()
    api.state.assets = api.state.assets.filter(asset => !asset.name.endsWith(extension))
    await expect(release.publishRelease(api, { ...options, id: 42 })).rejects.toThrow(/missing/i)
    expect(api.repos.updateRelease).not.toHaveBeenCalled()
  })

  it('does not accept installers from an older app version', async () => {
    const api = boundary()
    api.state.assets = api.state.assets.map(asset => ({ ...asset, name: asset.name.replace('0.5.0', '0.3.0') }))
    await expect(release.publishRelease(api, { ...options, id: 42 })).rejects.toThrow(/missing/i)
    expect(api.repos.updateRelease).not.toHaveBeenCalled()
  })

  it('does not publish empty installer files', async () => {
    const api = boundary()
    api.state.assets[0]!.size = 0
    await expect(release.publishRelease(api, { ...options, id: 42 })).rejects.toThrow(/missing/i)
    expect(api.repos.updateRelease).not.toHaveBeenCalled()
  })

  it('checks the release tag before publishing', async () => {
    const api = boundary()
    api.repos.getRelease.mockResolvedValue({ data: { ...api.draft, tag_name: 'v0.4.0' } })
    await expect(release.publishRelease(api, { ...options, id: 42 })).rejects.toThrow(/tag/i)
    expect(api.repos.updateRelease).not.toHaveBeenCalled()
  })

  it('publishes as latest only after all versioned installers are attached', async () => {
    const api = boundary()
    await release.publishRelease(api, { ...options, id: 42 })
    expect(api.repos.updateRelease).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ release_id: 42, draft: false, prerelease: false, make_latest: 'true' }))
    expect(api.core.summary.addLink).toHaveBeenCalledWith('Published release', api.draft.html_url)
  })
})
