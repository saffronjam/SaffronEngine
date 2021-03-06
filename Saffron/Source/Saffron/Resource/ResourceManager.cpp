#include "SaffronPCH.h"
#include "ResourceManager.h"

namespace Se
{
std::shared_ptr<Resource> &ResourceManager::Get(std::shared_ptr<Resource> resource)
{
	return Get(resource->GetIdentifier());
}

std::shared_ptr<Resource> &ResourceManager::Get(size_t identifer)
{
	return GetInstance()->m_Memory.at(identifer);
}

bool ResourceManager::Exists(std::shared_ptr<Resource> resource)
{
	return Exists(resource->GetIdentifier());
}

bool ResourceManager::Exists(size_t identifier)
{
	auto &instance = GetInstance();
	return instance->m_Memory.find(identifier) != instance->m_Memory.end();
}

void ResourceManager::Emplace(std::shared_ptr<Resource> resource)
{
	auto &instance = GetInstance();
	instance->m_Memory.emplace(resource->GetIdentifier(), resource);
}

void ResourceManager::Emplace(std::shared_ptr<Resource> resource, size_t identifier)
{
	auto &instance = GetInstance();
	instance->m_Memory.emplace(identifier, resource);
}

const ArrayList<std::shared_ptr<Resource>> &ResourceManager::GetAll()
{
	auto &instance = GetInstance();
	if ( instance->m_NeedCacheSync )
	{
		instance->SyncCache();
	}
	return GetInstance()->m_ReturnCache;
}

void ResourceManager::Clear()
{
	GetInstance()->m_Memory.clear();
	GetInstance()->m_ReturnCache.clear();
	GetInstance()->m_NeedCacheSync = false;
}

std::shared_ptr<ResourceManager> &ResourceManager::GetInstance()
{
	static std::shared_ptr<ResourceManager> s_ResourceManager = CreateShared<ResourceManager>();
	return s_ResourceManager;
}

void ResourceManager::SyncCache()
{
	m_ReturnCache.clear();
	m_ReturnCache.reserve(m_Memory.size());
	std::transform(m_Memory.begin(), m_Memory.end(), std::back_inserter(m_ReturnCache),
				   [](const auto &pair) { return pair.second; });
	m_NeedCacheSync = false;
}
}
