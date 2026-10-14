# Saved users and the Access check

Most access-control bugs show up when one user can see or do what another should not. **Saved users** keep each user's cookies and tokens, so you can send the same request as anyone in one click, and the **Access check** replays requests as each of them side by side.

Both are tools from the [Market](market.md) (under **Tools**) that come switched on, so the **Users** and **Access** screens are in the sidebar from the start. Remove either in the Market to hide it; it stays off until you install it again.

## Saving a user

- **Save login as a user** in the Lens, on a response that sets a session, keeps that user's cookies.
- **Found in traffic** on the Users screen lists the sessions your captured browser traffic carries (see below).
- **+ Add user** on the Users screen creates one by hand: paste a `Cookie` header, add cookies one by one, or add headers sent with every request as that user, such as a bearer token.

Values stay in the project. Nothing is sent until you browse, press Send or run a scan.

## Users found in traffic

Large apps rarely sign everything with one token: the pages use a session cookie, the API a bearer token, a back office another token again. Plonix reads the captured traffic to in-scope hosts and groups these into sessions, one per person: tokens sent together in one request, a token handed back in answer to a request signed with another, and tokens naming the same account belong together.

Each session lists what it sends and where: a cookie with its domain, a header with its host and, when a host takes different tokens under different paths, the path (for example `/v2/seller`). When a saved user already holds part of a session, Plonix says whose it is and offers **Add to** that user; otherwise **Save as a new user**, named from the account the traffic shows. Nothing is kept until you pick. The Lens offers the same on a request that carries such a session, and so does **Keep this session as a user…** in the Traffic row menu.

Only captured traffic counts: requests you sent from the Bench or Scans are experiments, not sessions. Tokens also sent to hosts outside scope are listed but not kept.

## Acting as a user

**Act as** in the title bar picks who you are. While you act as a user, browser traffic to in-scope hosts, the Bench and Scans are sent with that user's cookies, and cookies the server sets are kept with the user instead of in your browser. Pick **Your browser** to stop.

A Bench tab can pick its own user with **As …** next to Send. **As written** sends the request exactly as it is in the editor. Switching users sends nothing by itself and never widens scope.

## The Users screen

Each user lists their cookies and headers:

- edit, **Expire** or **Restore** a cookie, or remove it; **Expire all** signs the user out but keeps the cookies;
- headers can have a **Host** and **Path**: a header with a host is sent only to that host and its subdomains, with a path only under that path, and where two apply the longer path wins; a header with neither goes to every in-scope host;
- **Keep cookies fresh** updates a cookie here when the server sets or clears it in answer to a request sent as this user;
- **Open a browser as** the user gives them a browser window of their own, so you can be signed in as two users at once. What that window learns (cookies, bearer tokens) is saved with the user.

## Is a user still signed in?

Each user on the Users screen shows whether its session still works and what it may do, read from what Plonix already has. Nothing is sent to find out.

- **Session** says Working, Not expired, Expiring soon, Partly expired, Refused, Expired or Can't tell, with what that rests on: the expiry a token or cookie states (a JSON Web Token's `exp`, a cookie's expiry, or the `expires_in` of the answer that handed a token out), and how the server answered requests that carried the user's credentials. The list of users shows the same in one word.
- A user is called **Expired** only when every credential it sends has passed its stated expiry, and **Refused** only when the server turned the session away: a 401, or a redirect to a sign-in page, on an endpoint that took the same session before, a "who am I" endpoint, or with an answer that blames the token. A 403 is a missing permission, not an expired session. Where nothing says, it reads **Can't tell**.
- **Check now** sends one captured request, a "who am I" one where there is one, as that user, to settle it. It goes through the scope check and is recorded in Traffic.
- When the server still accepts a token after its stated expiry, Plonix points it out: an expired token should be refused.
- **What it may do** lists the roles, scopes, groups and permissions its tokens and "who am I" answers name, and the endpoints that allowed or refused requests sent as this user. Click one to open the request. To find out more, run the Access check as that user.

## The Access check

The Access check replays requests as each saved user and once **signed out** (auth headers removed), then lines up status, size and content for each.

1. Pick the requests: select rows in Traffic or endpoints on the [Map](map.md), or choose a host and, optionally, a branch of the application (paths starting with `/api/admin`, say). One captured request is checked for each endpoint on that branch.
2. Tick the users to compare, and whether to include signed out.
3. Press **Check access**.

The results point out where a signed-out request still succeeds and where different users get the same response. They draw no conclusions: you decide what they mean, and record a [finding](findings.md) with the requests as evidence. Every request goes through the scope check and is recorded in Traffic.

**Check this id across users** in the Lens, on a request with an id in its path, starts an Access check for that request in one click. **Replay signed out**, on a signed-in request that worked, sends it again with your login removed, to see whether it needs you signed in at all.
