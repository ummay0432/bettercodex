# Shopify GraphQL Docs Manifest

Use this file as a routing map for Shopify GraphQL work. Prefer the live Shopify docs linked here over copied notes so agents read the current schema, versioning guidance, limits, and examples.

When starting a task:

1. Identify the Shopify surface: Admin, Storefront, Customer Account, Partner, Payments Apps, or Functions.
2. Open that surface's overview first for authentication, endpoints, rate limits, and usage constraints.
3. Open the matching full index when you need the exact query, mutation, object, input, enum, scalar, payload, interface, or union page.
4. If production code targets a fixed API version, replace `/latest/` in the relevant URL with the configured version, such as `/2026-04/`.

## Core GraphQL API References

### Admin GraphQL API
Use when: Building apps and integrations that read or write Shopify admin data: products, orders, customers, inventory, metafields, discounts, billing, markets, fulfillment, and webhooks.
Overview: https://shopify.dev/docs/api/admin-graphql/latest
Full index: https://shopify.dev/docs/api/admin-graphql/latest/full-index

### Storefront API
Use when: Building buyer-facing storefront, cart, product, collection, search, localization, and checkout flows for web, mobile, apps, and games.
Overview: https://shopify.dev/docs/api/storefront/latest
Full index: https://shopify.dev/docs/api/storefront/latest/full-index

### Customer Account API
Use when: Building authenticated customer experiences for customer profile, orders, returns, addresses, subscriptions, and customer-scoped actions.
Overview: https://shopify.dev/docs/api/customer/latest
Full index: https://shopify.dev/docs/api/customer/latest/full-index

### Partner API
Use when: Automating Partner Dashboard data such as app events, partner transactions, organization data, app records, and theme records.
Overview: https://shopify.dev/docs/api/partner/latest
Full index: https://shopify.dev/docs/api/partner/latest/full-index

### Payments Apps API
Use when: Building approved payments apps that resolve, pend, reject, capture, refund, void, or verify payment sessions.
Overview: https://shopify.dev/docs/api/payments-apps/latest
Full index: https://shopify.dev/docs/api/payments-apps/latest/full-index

### Function APIs
Use when: Building Shopify Functions with GraphQL input queries and typed outputs for backend commerce customization.
Overview: https://shopify.dev/docs/api/functions/latest
Full index: See function references below.

## Shopify Function API References

Function API pages are large because they include target-specific GraphQL input and output schemas. Open the exact function surface before writing `run.graphql`, output operations, or `shopify.extension.toml` targeting.

### Cart and Checkout Validation
Use when: Blocking checkout or cart progress when business rules fail.
Reference: https://shopify.dev/docs/api/functions/latest/cart-and-checkout-validation

### Cart Transform
Use when: Expanding, merging, or transforming cart lines.
Reference: https://shopify.dev/docs/api/functions/latest/cart-transform

### Delivery Customization
Use when: Hiding, renaming, reordering, or selecting delivery options.
Reference: https://shopify.dev/docs/api/functions/latest/delivery-customization

### Discount
Use when: Generating product, order, shipping, or combined discounts.
Reference: https://shopify.dev/docs/api/functions/latest/discount

### Fulfillment Constraints
Use when: Adding fulfillment restrictions and constraints.
Reference: https://shopify.dev/docs/api/functions/latest/fulfillment-constraints

### Order Routing Location Rule
Use when: Customizing order routing location ranking.
Reference: https://shopify.dev/docs/api/functions/latest/order-routing-location-rule

### Payment Customization
Use when: Hiding, renaming, reordering, or selecting payment options.
Reference: https://shopify.dev/docs/api/functions/latest/payment-customization

## Cross-Cutting GraphQL Practices

Read these before changing query shape, pagination, rate-limit handling, identifiers, or API versions.

### About GraphQL at Shopify
Use when: Learning Shopify's GraphQL model and why new integrations should prefer GraphQL over REST.
Link: https://shopify.dev/docs/apps/build/graphql

### Queries
Use when: Reading data, using `QueryRoot`, fields, connections, and search filters.
Link: https://shopify.dev/docs/apps/build/graphql/basics/queries

### Mutations
Use when: Creating, updating, deleting, or invoking Shopify-side behavior.
Link: https://shopify.dev/docs/apps/build/graphql/basics/mutations

### Variables
Use when: Reusing query documents and passing dynamic values safely.
Link: https://shopify.dev/docs/apps/build/graphql/basics/variables

### Advanced GraphQL Concepts
Use when: Using aliases, fragments, inline fragments, and multi-operation documents.
Link: https://shopify.dev/docs/apps/build/graphql/basics/advanced

### Pagination
Use when: Implementing cursor-based pagination with connections, edges, nodes, and page info.
Link: https://shopify.dev/docs/api/usage/pagination-graphql

### Global IDs
Use when: Handling Shopify GIDs and object identity across APIs.
Link: https://shopify.dev/docs/api/usage/gids

### Bulk Operations
Use when: Moving high-volume Admin API reads and writes out of normal request paths.
Link: https://shopify.dev/docs/api/usage/bulk-operations

### Versioning
Use when: Choosing stable, release-candidate, unstable, or fixed API docs and understanding retirement windows.
Link: https://shopify.dev/docs/api/usage/versioning

### Limits and Rate Limits
Use when: Estimating query cost, input limits, pagination limits, throttle behavior, and debug headers.
Link: https://shopify.dev/docs/api/usage/limits

### Access Scopes
Use when: Choosing the least-privilege OAuth scopes required for Admin API access.
Link: https://shopify.dev/docs/api/usage/access-scopes

## Agent Routing Notes

- Use `latest` links when learning the current best practice or finding current schema docs.
- Use fixed-version links when matching code that already pins `api_version`.
- Start with overview pages for authentication and endpoint details; jump to full indexes only after the target surface is clear.
- For schema details, prefer full-index navigation over search results. The full indexes expose Shopify's generated API pages and are the least ambiguous way to locate exact GraphQL types.
- For performance-sensitive work, read limits and rate limits before broadening query fields or adding nested connections.
- For bulk data movement, read bulk operations before implementing loops over paginated Admin API results.
