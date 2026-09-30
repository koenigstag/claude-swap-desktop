// Sample state for dev/preview.html, shaped like the backend's get_state.
// Made-up accounts; regenerate from real output locally if needed, but don't commit real data.
window.MOCK_STATE = {
  "list": {
    "schemaVersion": 1,
    "activeAccountNumber": 2,
    "accounts": [
      {
        "number": 1,
        "email": "work@example.com",
        "organizationName": "Example Co",
        "organizationUuid": "00000000-0000-4000-8000-000000000001",
        "isOrganization": true,
        "active": false,
        "usageStatus": "ok",
        "usage": {
          "fiveHour": { "pct": 0.0, "resetsAt": "2026-09-30T20:10:00+03:00", "countdown": "4h 47m", "clock": "20:10" },
          "sevenDay": {
            "pct": 27.0, "resetsAt": "2026-10-04T16:00:00+03:00", "countdown": "4d 0h", "clock": "Oct 4 16:00",
            "expectedPct": 42.5, "aheadOfPace": false, "projectedExhaustionAt": "2026-10-08T13:13:27Z", "willLastToReset": true
          },
          "scoped": [
            {
              "pct": 0.0, "resetsAt": "2026-10-04T16:00:00+03:00", "countdown": "4d 0h", "clock": "Oct 4 16:00",
              "expectedPct": 42.5, "aheadOfPace": false, "willLastToReset": true, "name": "Fable"
            }
          ]
        },
        "usageFetchedAt": "2026-09-30T12:20:26Z",
        "usageAgeSeconds": 151.5
      },
      {
        "number": 2,
        "email": "personal@example.com",
        "organizationName": "personal@example.com's Organization",
        "organizationUuid": "00000000-0000-4000-8000-000000000002",
        "isOrganization": true,
        "active": true,
        "usageStatus": "ok",
        "usage": {
          "fiveHour": { "pct": 12.0, "resetsAt": "2026-09-30T19:10:00+03:00", "countdown": "3h 47m", "clock": "19:10" },
          "sevenDay": {
            "pct": 14.0, "resetsAt": "2026-10-03T14:00:00+03:00", "countdown": "2d 22h", "clock": "Oct 3 14:00",
            "expectedPct": 57.9, "aheadOfPace": false, "projectedExhaustionAt": "2026-10-25T10:17:22Z", "willLastToReset": true
          },
          "scoped": [
            {
              "pct": 0.0, "resetsAt": "2026-10-03T14:00:00+03:00", "countdown": "2d 22h", "clock": "Oct 3 14:00",
              "expectedPct": 57.9, "aheadOfPace": false, "willLastToReset": true, "name": "Fable"
            }
          ]
        },
        "usageFetchedAt": "2026-09-30T12:20:26Z",
        "usageAgeSeconds": 151.5
      }
    ],
    "unclaimedCredentials": []
  },
  "tokens": [
    {
      "number": 1,
      "email": "work@example.com",
      "active": false,
      "noCredentials": false,
      "lines": [
        { "source": "session profile", "state": "fresh", "refresh": true, "expires": "22:30 in 7h 9m" },
        { "source": "stored backup", "state": "fresh", "refresh": true, "expires": "22:30 in 7h 9m" }
      ]
    },
    {
      "number": 2,
      "email": "personal@example.com",
      "active": true,
      "noCredentials": false,
      "lines": [
        { "source": "active profile", "state": "fresh", "refresh": true, "expires": "22:02 in 6h 42m" }
      ]
    }
  ],
  "defaultLogin": {
    "loggedIn": true,
    "email": "personal@example.com",
    "orgName": "personal@example.com's Organization"
  },
  "mappings": [],
  "sessions": [
    { "pid": 1, "cwd": "C:\\example", "entrypoint": "claude-desktop" }
  ]
};
