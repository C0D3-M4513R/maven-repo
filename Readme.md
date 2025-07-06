This repo is intended to be a fast and lightweight implementation of a maven-repository capable of:
- transparently caching upstream repos
- hosting own repos, with the ability to add/deploy new items via PUT requests (and later to also support DELETE requests)

It's explicitly UNSUPPORTED to have a transparently cached repo, which can also be deployed to.

Currently, this is being hosted on https://reposilite.c0d3m4513r.com, with only HEAD and GET requests being served by this.
The code used for handling new deployments is currently 100% untested (outside of it compiling).