The opencode Zen backend checks for specific headers to identify official clients. Without these headers, requests are treated as anonymous and subjected to very restrictive rate limits (fallbackValue instead of dailyRequests ).

The official opencode CLI sends these headers when calling Zen:
- x-opencode-client: "cli"
- x-opencode-session: <random-id>
- x-opencode-project: <random-id>
- x-opencode-request: <random-id>
- User-Agent: "opencode/latest/1.3.15/cli"

We are not sending any of these headers, causing the backend to treat it as an unauthenticated/anonymous client.
