#!/bin/bash
set -e

GATEWAY="http://localhost:8888"

echo "============================================"
echo "  Full API Test Suite via Higress Gateway"
echo "============================================"

echo ""
echo "=== Step 1: Auth Service - Register ==="
REGISTER_RESP=$(curl -s "$GATEWAY/api/v1/auth/register" -X POST -H "Content-Type: application/json" -d '{"username":"apitest_full","email":"apitest_full@example.com","password":"password123"}')
echo "$REGISTER_RESP"

echo ""
echo "=== Step 2: Auth Service - Login ==="
LOGIN_RESP=$(curl -s "$GATEWAY/api/v1/auth/login" -X POST -H "Content-Type: application/json" -d '{"username":"apitest_full","password":"password123"}')
echo "$LOGIN_RESP"

TOKEN=$(echo "$LOGIN_RESP" | grep -o '"access_token":"[^"]*"' | cut -d'"' -f4)
REFRESH_TOKEN=$(echo "$LOGIN_RESP" | grep -o '"refresh_token":"[^"]*"' | cut -d'"' -f4)

if [ -z "$TOKEN" ]; then
    echo "❌ Failed to get token, aborting tests"
    exit 1
fi
echo "✅ Token obtained successfully"

echo ""
echo "=== Step 3: Auth Service - Refresh Token ==="
curl -s "$GATEWAY/api/v1/auth/refresh" -X POST -H "Content-Type: application/json" -d "{\"refresh_token\":\"$REFRESH_TOKEN\"}"

echo ""
echo "=== Step 4: PM Service - Health Check ==="
curl -s "$GATEWAY/health"

echo ""
echo "=== Step 5: PM Service - Create Project ==="
CREATE_RESP=$(curl -s "$GATEWAY/api/v1/projects" -X POST -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"name":"API Test Project","description":"Project created via Higress gateway test"}')
echo "$CREATE_RESP"

PROJECT_ID=$(echo "$CREATE_RESP" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
echo "Project ID: $PROJECT_ID"

echo ""
echo "=== Step 6: PM Service - List Projects ==="
curl -s "$GATEWAY/api/v1/projects" -H "Authorization: Bearer $TOKEN"

if [ -n "$PROJECT_ID" ] && [ "$PROJECT_ID" != "" ]; then
    echo ""
    echo "=== Step 7: PM Service - Get Project by ID ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID" -H "Authorization: Bearer $TOKEN"

    echo ""
    echo "=== Step 8: PM Service - Update Project ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID" -X PUT -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"name":"Updated API Test Project","description":"Updated description"}'

    echo ""
    echo "=== Step 9: PM Service - Add Member ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID/members" -X POST -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"user_id":"d2597536-fad3-45d1-91bb-4a7e68c1cd7b","role":"member"}'

    echo ""
    echo "=== Step 10: PM Service - List Members ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID/members" -H "Authorization: Bearer $TOKEN"

    echo ""
    echo "=== Step 11: PM Service - Create Work Package ==="
    WP_RESP=$(curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID/work-packages" -X POST -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"subject":"Test Work Package","description":"WP created via API test","type":"task"}')
    echo "$WP_RESP"

    WP_ID=$(echo "$WP_RESP" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
    echo "Work Package ID: $WP_ID"

    if [ -n "$WP_ID" ] && [ "$WP_ID" != "" ]; then
        echo ""
        echo "=== Step 12: PM Service - List Work Packages ==="
        curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID/work-packages" -H "Authorization: Bearer $TOKEN"

        echo ""
        echo "=== Step 13: PM Service - Get Work Package ==="
        curl -s "$GATEWAY/api/v1/work-packages/$WP_ID" -H "Authorization: Bearer $TOKEN"

        echo ""
        echo "=== Step 14: PM Service - Update Work Package ==="
        curl -s "$GATEWAY/api/v1/work-packages/$WP_ID" -X PUT -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"subject":"Updated Work Package","description":"Updated WP description"}'

        echo ""
        echo "=== Step 15: PM Service - Transition Status ==="
        curl -s "$GATEWAY/api/v1/work-packages/$WP_ID/transition" -X POST -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" -d '{"status":"in_progress"}'

        echo ""
        echo "=== Step 16: PM Service - Get Work Package History ==="
        curl -s "$GATEWAY/api/v1/work-packages/$WP_ID/history" -H "Authorization: Bearer $TOKEN"

        echo ""
        echo "=== Step 17: PM Service - Delete Work Package ==="
        curl -s "$GATEWAY/api/v1/work-packages/$WP_ID" -X DELETE -H "Authorization: Bearer $TOKEN"
    else
        echo "⚠️ Could not create work package, skipping WP tests"
    fi

    echo ""
    echo "=== Step 18: PM Service - Remove Member ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID/members/d2597536-fad3-45d1-91bb-4a7e68c1cd7b" -X DELETE -H "Authorization: Bearer $TOKEN"

    echo ""
    echo "=== Step 19: PM Service - Delete Project ==="
    curl -s "$GATEWAY/api/v1/projects/$PROJECT_ID" -X DELETE -H "Authorization: Bearer $TOKEN"
else
    echo "⚠️ Could not create project, skipping dependent tests"
fi

echo ""
echo "============================================"
echo "  All API Tests Completed!"
echo "============================================"