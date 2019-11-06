const core = require('@actions/core');
const path = require("path");
const fs = require("fs");
const github = require('@actions/github');
const glob = require('glob');

async function run() {
  // Load all our inputs and env vars. Note that `getInput` reads from `INPUT_*`
  const files = core.getInput('files');
  const name = core.getInput('name');
  const token = core.getInput('token');
  const slug = process.env.GITHUB_REPOSITORY;
  const owner = slug.split('/')[0];
  const repo = slug.split('/')[1];
  const sha = process.env.GITHUB_SHA;

  core.info(`files: ${files}`);
  core.info(`name: ${name}`);
  core.info(`token: ${token}`);

  const octokit = new github.GitHub(token);

  // If this is a `dev` release then we need to actually delete the previous
  // release since we can't overwrite a new one. We also need to update the
  // `dev` tag while we're at it. So here you'll see:
  //
  // * Look for the `dev` release, then delete it if it exists
  // * Update the `dev` release to our current sha, or create one if it doesn't
  //   exist
  if (name == 'dev') {
    const releases = await octokit.paginate("GET /repos/:owner/:repo/releases", { owner, repo });
    for (const release of releases) {
      if (release.tag_name !== 'dev') {
        continue;
      }
      const release_id = release.id;
      core.info(`deleting release ${release_id}`);
      await octokit.repos.deleteRelease({ owner, repo, release_id });
    }

    try {
      core.info(`updating dev tag`);
      await octokit.git.updateRef({
          owner,
          repo,
          ref: 'tags/dev',
          sha,
          force: true,
      });
    } catch (e) {
      console.log("ERROR: ", JSON.stringify(e, null, 2));
      core.info(`creating dev tag`);
      await octokit.git.createTag({
        owner,
        repo,
        tag: 'dev',
        message: 'dev release',
        object: sha,
        type: 'commit',
      });
    }
  }

  // Creates an official GitHub release for this `tag`, and if this is `dev`
  // then we know that from the previous block this should be a fresh release.
  core.info(`creating a release`);
  const release = await octokit.repos.createRelease({
    owner,
    repo,
    tag_name: name,
    prerelease: name === 'dev',
  });

  // Upload all the relevant assets for this release as just general blobs.
  for (const file of glob.sync(files)) {
    const size = fs.statSync(file).size;
    core.info(`upload ${file}`);
    await octokit.repos.uploadReleaseAsset({
      file: fs.createReadStream(file),
      headers: { 'content-length': size, 'content-type': 'application/octet-stream' },
      name: path.basename(file),
      url: release.data.upload_url,
    })
  }
}

run().catch(err => {
  console.log("ERROR: ", JSON.stringify(err, null, 2));
  core.setFailed(err.message);
  console.log(err.stack);
});
