---
nessemble: none
---
Fix the Publish pipeline's release step: stop the container-image job from uploading build-push-action's `.dockerbuild` build-record artifact, which the release job's unfiltered artifact download choked on.
