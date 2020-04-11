# devtodo

A simple tool to synchronize issue and pull request statuses from GitHub. It
stores information locally using ical files with `VTODO` items.

They may be viewed using tools such as [todoman][] or any other calendaring
software which can read ical files from a directory. They may be synced to
cloud services using tools such as [vdirsyncer][] as well.

[todoman]: https://github.com/pimutils/todoman
[vdirsyncer]: https://github.com/pimutils/vdirsyncer

## Future plans

  - Better filtering
  - GitLab support
  - Fetching information from specific repositories only
  - Cached "last fetched" information
    - GitHub supports "only changed since" filtering on queries which should
      improve performance.
