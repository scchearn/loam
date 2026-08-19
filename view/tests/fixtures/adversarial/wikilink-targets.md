# Hostile link targets

[[../../etc/passwd]]
[[//evil.example.com/x]]
[[..%2f..%2fetc%2fpasswd]]
[[Alpha Page|<img src=x onerror=window.__pwned=1>]]
[[Alpha Page#<img src=x onerror=window.__pwned=1>]]

[climbing](../../../../etc/passwd.md)
[protocol relative](//evil.example.com/x.md)
[backslash](\\evil.example.com\x.md)
[percent traversal](%2e%2e/%2e%2e/etc/passwd.md)
[reader route](#/reader/..%2F..%2F..%2Fetc%2Fpasswd)

<a href="#/reader/..%2f..%2fetc%2fpasswd">raw reader route</a>
