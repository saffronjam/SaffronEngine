#include "SaffronPCH.h"

#include "Saffron/Gui/Gui.h"
#include "Saffron/Math/SaffronMath.h"
#include "Saffron/Core/Misc.h"
#include "Saffron/Scene/SceneCamera.h"
#include "Saffron/Rendering/SceneRenderer.h"

namespace Se
{
SceneCamera::SceneCamera(Uint32 width, Uint32 height, ProjectionMode projectionType)
{
	SceneCamera::SetViewportSize(width, height);
}


void SceneCamera::RenderFrustum(const Matrix4f& transform) const
{
	const float nearClip = GetPerspectiveNearClip();
	const float farClip = GetPerspectiveFarClip();
	const float fov = GetPerspectiveVerticalFOV();
	const float ratio = 16.0f / 9.0f;

	auto [translation, rotation, scale] = Misc::GetTransformDecomposition(transform);

	Vector3f& position = translation;
	Vector3f forward = normalize(rotate(rotation, {0.0f, 0.0f, -1.0f}));
	Vector3f right = normalize(cross(forward, {0.0f, 1.0f, 0.0f}));
	Vector3f up = normalize(cross(right, forward));

	float Hnear = 2.0f * std::tan(fov / 2) * nearClip;
	float Wnear = Hnear * ratio;

	float Hfar = 2.0f * std::tan(fov / 2) * farClip;
	float Wfar = Hfar * ratio;

	Vector3f middleNearPlane = position + forward * nearClip;
	Vector3f middleFarPlane = position + forward * farClip;

	Vector3f topRightNear = middleNearPlane + up * Hnear / 2.0f + right * Wnear / 2.0f;
	Vector3f botRightNear = middleNearPlane - up * Hnear / 2.0f + right * Wnear / 2.0f;
	Vector3f botLeftNear = middleNearPlane - up * Hnear / 2.0f - right * Wnear / 2.0f;
	Vector3f topLeftNear = middleNearPlane + up * Hnear / 2.0f - right * Wnear / 2.0f;

	Vector3f topRightFar = middleFarPlane + up * Hfar / 2.0f + right * Wfar / 2.0f;
	Vector3f botRightFar = middleFarPlane - up * Hfar / 2.0f + right * Wfar / 2.0f;
	Vector3f botLeftFar = middleFarPlane - up * Hfar / 2.0f - right * Wfar / 2.0f;
	Vector3f topLeftFar = middleFarPlane + up * Hfar / 2.0f - right * Wfar / 2.0f;

	auto orange = Gui::GetSaffronOrange();
	auto orangeFade1 = Gui::GetSaffronOrange(0.5f);
	auto orangeFade2 = Gui::GetSaffronOrange(0.2f);

	//// Square near
	//SceneRenderer::SubmitLine(topRightNear, botRightNear, orange);
	//SceneRenderer::SubmitLine(botRightNear, botLeftNear, orange);
	//SceneRenderer::SubmitLine(botLeftNear, topLeftNear, orange);
	//SceneRenderer::SubmitLine(topLeftNear, topRightNear, orange);
	//// Square far
	//SceneRenderer::SubmitLine(topRightFar, botRightFar, orange);
	//SceneRenderer::SubmitLine(botRightFar, botLeftFar, orange);
	//SceneRenderer::SubmitLine(botLeftFar, topLeftFar, orange);
	//SceneRenderer::SubmitLine(topLeftFar, topRightFar, orange);
	//// Line from position to near
	//SceneRenderer::SubmitLine(translation, topRightNear, orangeFade2);
	//SceneRenderer::SubmitLine(translation, botRightNear, orangeFade2);
	//SceneRenderer::SubmitLine(translation, botLeftNear, orangeFade2);
	//SceneRenderer::SubmitLine(translation, topLeftNear, orangeFade2);
	//// Lines from near to far
	//SceneRenderer::SubmitLine(translation, topRightFar, orangeFade1);
	//SceneRenderer::SubmitLine(translation, botRightFar, orangeFade1);
	//SceneRenderer::SubmitLine(translation, botLeftFar, orangeFade1);
	//SceneRenderer::SubmitLine(translation, topLeftFar, orangeFade1);
}

void SceneCamera::SetPerspective(float verticalFOV, float nearClip, float farClip)
{
	_projectionMode = ProjectionMode::Perspective;
	_perspectiveFOV = verticalFOV;
	_perspectiveNear = nearClip;
	_perspectiveFar = farClip;
}

void SceneCamera::SetOrthographic(float size, float nearClip, float farClip)
{
	_projectionMode = ProjectionMode::Orthographic;
	_orthographicSize = size;
	_orthographicNear = nearClip;
	_orthographicFar = farClip;
}

void SceneCamera::SetViewportSize(Uint32 width, Uint32 height)
{
	Camera::SetViewportSize(width, height);
	switch (_projectionMode)
	{
	case ProjectionMode::Perspective: _projectionMatrix = glm::perspectiveFov(
			_perspectiveFOV, static_cast<float>(width), static_cast<float>(height), _perspectiveNear,
			_perspectiveFar);
		break;

	case ProjectionMode::Orthographic: const float aspect = static_cast<float>(width) / static_cast<float>(height);
		const float Width = _orthographicSize * aspect;
		const float Height = _orthographicSize;
		_projectionMatrix = glm::ortho(-Width * 0.5f, Width * 0.5f, -Height * 0.5f, Height * 0.5f);
		break;
	}
}
}