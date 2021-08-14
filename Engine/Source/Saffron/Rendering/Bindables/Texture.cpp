﻿#include "SaffronPCH.h"

#include "Saffron/Rendering/Bindables/Texture.h"

#include <DirectXTK/WICTextureLoader.h>

#include "Saffron/ErrorHandling/ExceptionHelpers.h"
#include "Saffron/Rendering/Renderer.h"

namespace Se
{
Texture::Texture(uint slot) :
	_slot(slot)
{
}

Texture::Texture(uint width, uint height, ImageFormat format, uint slot) :
	_format(format),
	_width(width),
	_height(height),
	_slot(slot)
{
	SetInitializer(
		[this]
		{
			const auto inst = ShareThisAs<Texture>();
			Renderer::Submit(
				[inst](const RendererPackage& package)
				{
					D3D11_TEXTURE2D_DESC td = {};
					td.Width = inst->_width;
					td.Height = inst->_height;
					td.Format = Utils::ToDxgiTextureFormat(inst->_format);
					td.ArraySize = 1;
					td.MipLevels = 1;
					td.MiscFlags = 0;
					td.SampleDesc.Count = 1;
					td.Usage = D3D11_USAGE_DEFAULT;
					td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
					td.CPUAccessFlags = 0;

					auto hr = package.Device.CreateTexture2D(&td, nullptr, &inst->_nativeTexture);
					ThrowIfBad(hr);

					D3D11_SHADER_RESOURCE_VIEW_DESC srvd = {};
					srvd.Format = td.Format;
					srvd.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
					srvd.Texture2D.MipLevels = td.MipLevels;
					srvd.Texture2D.MostDetailedMip = 0;

					hr = package.Device.CreateShaderResourceView(
						inst->_nativeTexture.Get(),
						&srvd,
						&inst->_nativeShaderResourceView
					);
					ThrowIfBad(hr);
				}
			);
		}
	);
}

Texture::Texture(const std::filesystem::path& path, uint slot) :
	_slot(slot),
	_path(std::make_optional(path))
{
	SetInitializer(
		[this]
		{
			const auto inst = ShareThisAs<Texture>();
			Renderer::Submit(
				[inst](const RendererPackage& package)
				{
					// Load from disk
					ComPtr<ID3D11Resource> texture;
					const auto hr = DirectX::CreateWICTextureFromFile(
						&package.Device,
						inst->_path->c_str(),
						&texture,
						&inst->_nativeShaderResourceView
					);
					ThrowIfBad(hr);
					texture->QueryInterface(__uuidof(ID3D11Texture2D), &inst->_nativeTexture);

					// Fetch texture info
					D3D11_TEXTURE2D_DESC td;
					inst->_nativeTexture->GetDesc(&td);

					inst->_format = Utils::ToSaffronFormat(td.Format);
					inst->_width = td.Width;
					inst->_height = td.Height;
				}
			);
		}
	);
}

Texture::Texture(const std::shared_ptr<Image>& image, uint slot) :
	_slot(slot)
{
	auto imageInst = image;
	SetInitializer(
		[this, imageInst]
		{
			const auto inst = ShareThisAs<Texture>();
			Renderer::Submit(
				[inst, imageInst](const RendererPackage& package)
				{
					const auto usage = imageInst->Usage();

					if (usage & ImageUsage_ShaderResource)
					{
						inst->_nativeShaderResourceView = imageInst->_nativeShaderResourceView;
					}
					inst->_nativeTexture = imageInst->_nativeTexture;

					inst->_width = imageInst->Width();
					inst->_height = imageInst->Height();
					inst->_format = imageInst->Format();
				}
			);
		}
	);
}

void Texture::Bind() const
{
	const auto inst = ShareThisAs<const Texture>();
	Renderer::Submit(
		[inst](const RendererPackage& package)
		{
			package.Context.PSSetShaderResources(inst->_slot, 1, inst->_nativeShaderResourceView.GetAddressOf());
		}
	);
}

void Texture::Unbind() const
{
	const auto inst = ShareThisAs<const Texture>();
	Renderer::Submit(
		[inst](const RendererPackage& package)
		{
			ID3D11ShaderResourceView* srv = nullptr;
			package.Context.PSSetShaderResources(inst->_slot, 1, &srv);
		}
	);
}

auto Texture::Width() const -> uint
{
	return _width;
}

auto Texture::Height() const -> uint
{
	return _height;
}

void Texture::SetImage(const std::shared_ptr<Image>& image)
{
	const auto imageInst = image;
	const auto inst = ShareThisAs<Texture>();
	Renderer::Submit(
		[inst, imageInst](const RendererPackage& package)
		{
			const auto usage = imageInst->Usage();

			if (usage & ImageUsage_ShaderResource)
			{
				inst->_nativeShaderResourceView = imageInst->_nativeShaderResourceView;
			}
			inst->_nativeTexture = imageInst->_nativeTexture;

			inst->_width = imageInst->Width();
			inst->_height = imageInst->Height();
			inst->_format = imageInst->Format();
		}
	);
}

auto Texture::NativeHandle() -> ID3D11Texture2D&
{
	return *_nativeTexture.Get();
}

auto Texture::NativeHandle() const -> const ID3D11Texture2D&
{
	return *_nativeTexture.Get();
}

auto Texture::ShaderView() -> ID3D11ShaderResourceView&
{
	return *_nativeShaderResourceView.Get();
}

auto Texture::ShaderView() const -> const ID3D11ShaderResourceView&
{
	return *_nativeShaderResourceView.Get();
}

auto Texture::Create(uint slot) -> std::shared_ptr<Texture>
{
	return BindableStore::Add<Texture>(slot);
}

auto Texture::Create(uint width, uint height, ImageFormat format, uint slot) -> std::shared_ptr<Texture>
{
	return BindableStore::Add<Texture>(width, height, format, slot);
}

auto Texture::Create(const std::filesystem::path& path, uint slot) -> std::shared_ptr<Texture>
{
	return BindableStore::Add<Texture>(path, slot);
}

auto Texture::Create(const std::shared_ptr<Image>& image, uint slot) -> std::shared_ptr<Texture>
{
	return BindableStore::Add<Texture>(image, slot);
}
}
