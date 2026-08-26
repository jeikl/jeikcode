package com.example.demo.service;

import com.example.demo.model.User;
import com.example.demo.mapper.UserMapper;
import org.springframework.stereotype.Service;
import java.util.List;

@Service
public class UserService {
    private final UserMapper userMapper;

    public UserService(UserMapper userMapper) {
        this.userMapper = userMapper;
    }

    public List<User> getAllUsers() {
        return userMapper.selectAll();
    }

    public void saveUser(User user) {
        userMapper.insertUser(user);
    }
}
