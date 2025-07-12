### About

This repo is intended to be a fast and lightweight implementation of a maven-repository capable of:
- transparently caching upstream repos
- hosting own repos, with the ability to add/deploy new items via PUT requests (and later to also support DELETE requests)

It's explicitly UNSUPPORTED to have a transparently cached repo, which can also be deployed to.

Currently, this is being hosted on https://reposilite.c0d3m4513r.com, with only HEAD and GET requests being served by this.
The code used for handling new deployments is currently 100% untested (outside of it compiling).

-------------

### Licensing

This repo has NO ASSOCIATED LICENSE.
This means that by default you should assume that I am not granting any rights, outside of those specified in the [GitHub Terms of Service](https://docs.github.com/en/site-policy/github-terms/github-terms-of-service#3-ownership-of-content-right-to-post-and-license-grants).

To summarise the rights granted by the GitHub Terms of Service (just a summary. Don't take this as legally binding):
- You may fork the repo
- You may view the Source-Code

But most notably: They don't include the rights to run the Source-Code.

Henceforth, "I" will refer to [C0D3-M4513R](https://github.com/C0D3-M4513R).
I will allow extra rights, as described below:
- I agree to anyone having obtained a non-modified copy of this software through git (where the latest commit is by [C0D3-M4513R](https://github.com/C0D3-M4513R), shown as verified on GitHub and the commit signature is not by any of GitHub's keys), to compile the software using a non-modified official stable cargo release (with the same non-modified official stable release also applying to any downstream dependency of cargo) or to compile the software with a non-modified official stable release version of [nix](https://github.com/nixOS/nix) using the provided `flake.lock` and `flake.nix` files.
- I allow artifacts, which were produced in accordance to the point above, to be executed as-is and without modifications using either the raw binary or the docker image using a non-modified official release version of docker in a non-malicious environment (e.g. don't modify the kernel to make this software do anything it wasn't designed to do, don't hook functions using dll-injection).
- Any deliberate or malicious mis-interpretation of these extra granted rights voids the extra granted rights.

If you want to modify the source-code, you can only do so, if you follow all these bullet points:
- you only compile the modified source-code in accordance to the above rights (with the exception, that modifications to the source code are allowed and that the git commit restrictions on the source-code of this repository are lifted).
- you may only execute the produced artifacts of the modified source-code for a limited time for testing and only if you make sure that nothing outside the computer, machine or environment (whichever is smaller) can contact the executed modified software.
- you do not distribute, make available or in any other way, shape or form share any of the modifications made outside the Private Security Advisory Issues on [this repo](https://github.com/C0D3-M4513R/maven-repo): https://github.com/C0D3-M4513R/maven-repo/security/advisories/new.